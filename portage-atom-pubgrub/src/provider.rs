use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Dep, UseDefault, UseDepKind, Version};
use crate::repository::IUseDefault;
use crate::use_config::UseFlagState;
use pubgrub::{
    Dependencies, DependencyConstraints, DependencyProvider, PackageResolutionStatistics,
    SelectedDependencies,
};

use crate::convert;
use crate::error::Error;
use crate::package::PortagePackage;
use crate::repository::PackageRepository;
use crate::use_config::UseConfig;
use crate::version_set::PortageVersionSet;

/// Whether an installed package should be favored or locked during resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstalledPolicy {
    /// Prefer the installed version when multiple candidates exist,
    /// but allow upgrades if required by dependencies.
    Favor,
    /// The installed version must not change — only that exact version
    /// is acceptable.
    Lock,
}

/// All solver-relevant data for one package version.
///
/// Previously this was eight parallel `BTreeMap<Version, _>` fields on
/// `PackageData`; collapsing them into one struct keeps a version's data
/// cohesive and removes the hand-synced map inserts.
pub(crate) struct VersionData {
    /// Merged deps for PubGrub's DependencyProvider trait.
    pub(crate) merged: Dependencies<PortagePackage, PortageVersionSet, String>,
    /// Per-class converted deps with optional gating USE flag.
    /// Index: 0=DEPEND, 1=RDEPEND, 2=BDEPEND, 3=PDEPEND, 4=IDEPEND
    pub(crate) by_class: Vec<Vec<(PortagePackage, PortageVersionSet, Option<Interned<DefaultInterner>>)>>,
    pub(crate) blockers: Vec<Dep>,
    pub(crate) use_deps: Vec<convert::UseDepConstraint>,
    pub(crate) iuse: Vec<Interned<DefaultInterner>>,
    pub(crate) iuse_defaults: HashMap<Interned<DefaultInterner>, IUseDefault>,
    pub(crate) repo: Option<Interned<DefaultInterner>>,
    pub(crate) repo_constraints: Vec<convert::RepoConstraint>,
    pub(crate) slot_operator_deps: Vec<convert::SlotOperatorDep>,
    /// The resolved **desired** USE state for this version: `package.use` and
    /// global USE applied on top of the ebuild's IUSE defaults.  This is the
    /// single source of truth for "is flag F on for this version" during both
    /// branch conversion and the post-solve passes.
    pub(crate) desired: UseConfig,
}

impl VersionData {
    /// Build a deps-only version (no blockers/use-deps/etc.), used for synthetic
    /// solver nodes: the root target set and OR-group / USE-decision branches.
    /// `merged` is collected from a flattened view of `by_class` (flag stripped).
    fn from_by_class(by_class: Vec<Vec<(PortagePackage, PortageVersionSet, Option<Interned<DefaultInterner>>)>>) -> Self {
        let merged = Dependencies::Available(
            by_class.iter().flatten().map(|(p, vs, _)| (p.clone(), vs.clone())).collect()
        );
        Self {
            merged,
            by_class,
            blockers: Vec::new(),
            use_deps: Vec::new(),
            iuse: Vec::new(),
            iuse_defaults: HashMap::new(),
            repo: None,
            repo_constraints: Vec::new(),
            slot_operator_deps: Vec::new(),
            desired: UseConfig::new(),
        }
    }
}

pub(crate) struct PackageData {
    pub(crate) versions: BTreeMap<Version, VersionData>,
}

/// A package that is already installed, with its version and policy.
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    /// The installed package identity.
    pub package: PortagePackage,
    /// The installed version.
    pub version: Version,
    /// How to treat this package during resolution.
    pub policy: InstalledPolicy,
    /// USE flags that were active (enabled) when this package was built.
    ///
    /// Used to evaluate USE dep constraints on OR-group branches so the solver
    /// can prefer branches that are already satisfied without a rebuild.
    pub active_use: Vec<Interned<DefaultInterner>>,
    /// IUSE flags declared by this installed package (flag names without `+`/`-` prefix).
    ///
    /// Required because the repository may not carry the exact installed version
    /// any more (e.g. glib-2.84.4-r2 installed while the repo only has r5).  In
    /// that case `PackageData.iuse` has no entry for the installed version, so we
    /// fall back to the VDB-recorded IUSE to avoid false-positive reinstall reports.
    pub iuse: Vec<Interned<DefaultInterner>>,
}

/// Build a per-CPV `UseConfig` by starting from the global config and applying
/// Apply per-package USE flag overrides on top of a base [`UseConfig`].
///
/// Scans `package_use` in order and applies any entries whose atom matches
/// `cpv`.  Returns `Borrowed(base)` when no entries match to avoid a clone.
pub fn apply_package_use<'a>(
    base: &'a UseConfig,
    cpv: &portage_atom::Cpv,
    slot: Option<Interned<DefaultInterner>>,
    package_use: &[(Dep, Vec<String>)],
) -> Cow<'a, UseConfig> {
    if package_use.is_empty() {
        return Cow::Borrowed(base);
    }
    let mut cfg = base.clone();
    for (dep, flags) in package_use {
        if crate::validate::dep_matches_cpv(dep, cpv, slot) {
            for flag in flags {
                let name = flag.strip_prefix('+').unwrap_or(flag);
                if let Some(stripped) = name.strip_prefix('-') {
                    cfg.disable(portage_atom::interner::Interned::intern(stripped));
                } else {
                    cfg.enable(portage_atom::interner::Interned::intern(name));
                }
            }
        }
    }
    Cow::Owned(cfg)
}

/// A dependency that was dropped because no versions were available.
///
/// Dropped deps are always alternatives inside an `||` dep group — a
/// successful resolution means the other branch was chosen instead.
#[derive(Debug, Clone)]
pub struct DroppedDep {
    /// The package that was dropped.
    pub package: PortagePackage,
    /// The version range that was requested.
    pub version_set: PortageVersionSet,
    /// Other real packages in the same `||` group that were available.
    /// Empty when the dep was not inside a `||` (direct unconditional dep).
    pub alternatives: Vec<PortagePackage>,
}

/// USE flag changes required on a package by the resolved dependency set.
///
/// Produced by the post-solve validation pass in
/// [`PortageDependencyProvider::resolve_targets`].
///
/// For **installed** packages the required changes were not yet applied, so the
/// package must be rebuilt — this corresponds to portage's `R` action.
///
/// For **new** packages the required flags should be set when the package is
/// built.  Since our solver does not yet enforce USE dep constraints at build
/// time, these are reported as informational annotations.
#[derive(Debug, Clone)]
pub struct UseFlagRequirement {
    /// The package the requirements apply to.
    pub package: PortagePackage,
    /// The currently-installed (or selected) version.
    pub version: Version,
    /// If set, the package should be **upgraded** to this version rather than
    /// rebuilt at `version`.  Present when the installed version is superseded
    /// by a newer repo version whose constraints drove the requirement.
    pub upgrade_to: Option<Version>,
    /// USE flags that must be **enabled** — required by at least one constraint
    /// but not yet active (installed: violated now; new: may not be set by config).
    pub required_enabled: Vec<Interned<DefaultInterner>>,
    /// USE flags that must be **disabled** — forbidden by at least one constraint
    /// but currently active.
    pub required_disabled: Vec<Interned<DefaultInterner>>,
    /// The package(s) that imposed the USE dep constraints (CPN strings).
    /// Used to generate `package.use` comments.
    pub required_by: Vec<String>,
}

/// A PubGrub `DependencyProvider` backed by a portage package repository.
///
/// Pre-computes all dependency information at construction time, then serves
/// it to the PubGrub solver.
pub struct PortageDependencyProvider {
    pub(crate) packages: HashMap<PortagePackage, PackageData>,
    pub(crate) installed: HashMap<PortagePackage, (Version, InstalledPolicy)>,
    pub(crate) installed_cpns: HashSet<Cpn>,
    pub(crate) installed_use: HashMap<PortagePackage, Vec<Interned<DefaultInterner>>>,
    pub(crate) installed_iuse: HashMap<PortagePackage, Vec<Interned<DefaultInterner>>>,
    pub(crate) dropped_deps: Vec<DroppedDep>,
    /// Global USE configuration, still consulted by the OR-branch selection
    /// heuristic (`use_dep_branch_satisfied`); the post-solve passes use the
    /// per-version `VersionData::desired` instead.
    pub(crate) use_config: UseConfig,
    /// USE flag requirements collected by the post-solve validation pass.
    ///
    /// Covers both reinstall cases (`R`: installed packages with violated
    /// constraints) and informational cases (`N`/`U`: new packages whose
    /// required flags may not be set by the current global config).
    pub(crate) use_flag_requirements: Vec<UseFlagRequirement>,
}

impl PortageDependencyProvider {
    /// Build the provider from a repository, a global USE flag configuration,
    /// and per-package USE overrides (from `package.use` / `package.use.force`).
    ///
    /// `package_use` is a list of `(atom, flags)` pairs applied in order; a
    /// flag prefixed with `-` disables it, a bare or `+`-prefixed flag enables
    /// it.  Entries are matched against each CPV using the atom's version
    /// constraint, so `>=dev-libs/foo-2.0 flag` only affects matching versions.
    pub fn new<R: PackageRepository>(
        repo: R,
        use_config: UseConfig,        package_use: &[(Dep, Vec<String>)],
    ) -> Self {
        let mut packages = HashMap::new();
        let mut cpn_slots: HashMap<portage_atom::Cpn, Vec<Interned<DefaultInterner>>> =
            HashMap::new();

        // First pass: collect slots per CPN directly from version metadata.
        // This ensures slots are derived from the same filtered data that
        // versions_for provides, avoiding phantom slots for live/9999 ebuilds.
        for cpn in repo.all_packages() {
            let versions = repo.versions_for(&cpn);
            let mut slots: Vec<Interned<DefaultInterner>> =
                versions.iter().filter_map(|(_, meta)| meta.slot).collect();
            slots.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            slots.dedup();
            if !slots.is_empty() {
                cpn_slots.insert(cpn, slots);
            }
        }

        // Build the slot map for convert_deps.
        let slot_map: convert::SlotMap = cpn_slots
            .iter()
            .map(|(&cpn, slots)| {
                let slot_packages = slots
                    .iter()
                    .map(|&s| (s, PortagePackage::slotted(cpn, s)))
                    .collect();
                (cpn, slot_packages)
            })
            .collect();

        // Second pass: register versions and convert deps.
        for cpn in repo.all_packages() {
            let versions_data = repo.versions_for(&cpn);

            for (cpv, meta) in versions_data {
                let pkg = match &meta.slot {
                    Some(slot) => PortagePackage::slotted(cpn, *slot),
                    None => {
                        if let Some([(_, sole_pkg)]) = slot_map.get(&cpn).map(|v| v.as_slice()) {
                            sole_pkg.clone()
                        } else {
                            PortagePackage::unslotted(cpn)
                        }
                    }
                };

                let cpn_str = format!("{}/{}", cpn.category, cpn.package);

                let dep_classes: [&[portage_atom::DepEntry]; 5] = [
                    &meta.deps.depend,
                    &meta.deps.rdepend,
                    &meta.deps.bdepend,
                    &meta.deps.pdepend,
                    &meta.deps.idepend,
                ];

                // The desired USE set for this version: package.use + global USE
                // overlaid on the ebuild's IUSE defaults.  Folding the defaults in
                // here makes `cpv_use_cfg` authoritative, so every later reader
                // (convert + post-solve) consults one resolved set.
                let mut cpv_use_cfg =
                    apply_package_use(&use_config, &cpv, meta.slot, package_use).into_owned();
                for (flag, def) in &meta.iuse_defaults {
                    if cpv_use_cfg.get_opt(flag).is_none() {
                        cpv_use_cfg.set(
                            *flag,
                            match def {
                                IUseDefault::Enabled => UseFlagState::Enabled,
                                IUseDefault::Disabled => UseFlagState::Disabled,
                            },
                        );
                    }
                }

                let class_results: [convert::ConversionResult; 5] = dep_classes.map(|entries| {
                    convert::convert_deps(
                        entries,
                        &cpn_str,
                        &cpv_use_cfg,
                        &slot_map,
                        &meta.iuse_defaults,
                    )
                });

                let mut all_blockers = Vec::new();
                let mut all_use_deps = Vec::new();
                let mut all_repo_constraints = Vec::new();
                let mut all_virtual_choices = Vec::new();
                let mut all_slot_operator_deps = Vec::new();
                let mut by_class: Vec<Vec<(PortagePackage, PortageVersionSet, Option<Interned<DefaultInterner>>)>> =
                    Vec::with_capacity(5);

                for result in class_results {
                    all_blockers.extend(result.blockers);
                    all_use_deps.extend(result.use_deps);
                    all_repo_constraints.extend(result.repo_constraints);
                    all_virtual_choices.extend(result.virtual_choices);
                    all_slot_operator_deps.extend(result.slot_operator_deps);
                    by_class.push(result.requirements);
                }

                let mut version_data = VersionData::from_by_class(by_class);
                version_data.blockers = all_blockers;
                version_data.use_deps = all_use_deps;
                version_data.iuse = meta.iuse;
                version_data.iuse_defaults = meta.iuse_defaults;
                version_data.repo = meta.repo;
                version_data.repo_constraints = all_repo_constraints;
                version_data.slot_operator_deps = all_slot_operator_deps;
                version_data.desired = cpv_use_cfg;

                let entry = packages
                    .entry(pkg)
                    .or_insert_with(|| PackageData { versions: BTreeMap::new() });
                entry.versions.insert(cpv.version.clone(), version_data);

                register_virtual_choices(&mut packages, all_virtual_choices);
            }
        }

        // Post-process: remove dependencies on packages not present in the
        // repository.  Without this filtering, PubGrub will encounter
        // `NoVersions` for any missing package and immediately declare the
        // problem unsolvable.
        let known: HashSet<PortagePackage> = packages.keys().cloned().collect();

        // Build a map from each real package to the other real packages in the
        // same || group (Choice node).  Used to populate DroppedDep::alternatives.
        let mut or_alternatives: HashMap<PortagePackage, Vec<PortagePackage>> = HashMap::new();
        for (pkg, data) in packages.iter_mut() {
            if !matches!(pkg, PortagePackage::Choice { .. }) {
                continue;
            }
            let mut branch_deps: Vec<PortagePackage> = Vec::new();
            for vd in data.versions.values_mut() {
                if let Dependencies::Available(constraints) = &mut vd.merged {
                    let taken = std::mem::take(constraints);
                    let items: Vec<_> = taken.into_iter().collect();
                    for (dep, _) in &items {
                        if !dep.is_virtual() {
                            branch_deps.push(dep.clone());
                        }
                    }
                    *constraints = items.into_iter().collect();
                }
            }
            for i in 0..branch_deps.len() {
                let others: Vec<_> = branch_deps
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, d)| d.clone())
                    .collect();
                or_alternatives
                    .entry(branch_deps[i].clone())
                    .or_default()
                    .extend(others);
            }
        }

        let mut dropped_deps = Vec::new();
        for data in packages.values_mut() {
            for vd in data.versions.values_mut() {
                if let Dependencies::Available(constraints) = &mut vd.merged {
                    let taken = std::mem::take(constraints);
                    let (kept, dropped): (Vec<_>, Vec<_>) =
                        taken.into_iter().partition(|(pkg, _)| known.contains(pkg));
                    dropped_deps.extend(dropped.into_iter().map(|(pkg, vs)| {
                        let alternatives = or_alternatives
                            .get(&pkg)
                            .map(|alts| {
                                alts.iter().filter(|a| known.contains(a)).cloned().collect()
                            })
                            .unwrap_or_default();
                        DroppedDep { package: pkg, version_set: vs, alternatives }
                    }));
                    *constraints = kept.into_iter().collect();
                }
                for class in &mut vd.by_class {
                    class.retain(|(pkg, _, _)| known.contains(pkg));
                }
            }
        }

        Self {
            packages,
            installed: HashMap::new(),
            installed_cpns: HashSet::new(),
            installed_use: HashMap::new(),
            installed_iuse: HashMap::new(),
            dropped_deps,
            use_config,
            use_flag_requirements: Vec::new(),
        }
    }

    /// Register an installed package.
    ///
    /// **Favored** packages are preferred during version selection but may be
    /// upgraded if a dependency requires it. **Locked** packages are pinned to
    /// their exact installed version.
    ///
    /// If the installed version is not present in the repository (e.g. an older
    /// version was removed from the tree), it is registered with empty
    /// dependencies so PubGrub can select it.  Without this, PubGrub would
    /// call `get_dependencies` for the installed version, receive `Unavailable`,
    /// and fall back to the newest repository version — defeating the Favor
    /// policy.
    pub fn add_installed(&mut self, installed: InstalledPackage) {
        self.installed_cpns.insert(*installed.package.cpn());

        // Ensure the installed version exists in self.packages so PubGrub can
        // use it even when it has been removed from the repository tree.
        if let Some(pkg_data) = self.packages.get_mut(&installed.package) {
            if !pkg_data.versions.contains_key(&installed.version) {
                let vd = VersionData::from_by_class(vec![vec![], vec![], vec![], vec![], vec![]]);
                pkg_data.versions.insert(installed.version.clone(), vd);
            }
        }

        if !installed.active_use.is_empty() {
            self.installed_use
                .insert(installed.package.clone(), installed.active_use);
        }
        if !installed.iuse.is_empty() {
            self.installed_iuse
                .insert(installed.package.clone(), installed.iuse);
        }
        self.installed
            .insert(installed.package, (installed.version, installed.policy));
    }

    /// Returns the list of dependencies that were dropped during construction
    /// because their target package was not present in the repository.
    ///
    /// Each entry is the `(package, version_set)` that could not be resolved.
    /// Callers should inspect this list to detect typos or genuinely missing
    /// packages rather than silently accepting an incomplete solution.
    pub fn dropped_deps(&self) -> &[DroppedDep] {
        &self.dropped_deps
    }

    /// Return all real (non-virtual, non-synthetic) packages in the provider
    /// whose CPN matches `cpn`.
    ///
    /// For packages with a single slot this returns one entry; for multi-slot
    /// packages (e.g. `dev-lang/python:3.11`, `dev-lang/python:3.12`) it
    /// returns one entry per slot.  Returns an empty vec if the CPN is not
    /// present in the repository.
    pub fn packages_for_cpn(&self, cpn: &portage_atom::Cpn) -> Vec<PortagePackage> {
        self.packages
            .keys()
            .filter(|p| !p.is_virtual() && p.cpn() == cpn)
            .cloned()
            .collect()
    }

    /// Return all versions registered for a given package, sorted ascending.
    pub fn versions_for_pkg(&self, pkg: &PortagePackage) -> Vec<Version> {
        self.packages
            .get(pkg)
            .map(|d| d.versions.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Return the merged dependency requirements for a specific package version,
    /// or `None` if the package/version is not registered.
    pub fn deps_for(
        &self,
        pkg: &PortagePackage,
        ver: &Version,
    ) -> Option<Vec<(PortagePackage, PortageVersionSet)>> {
        let data = self.packages.get(pkg)?;
        let vd = data.versions.get(ver)?;
        if let Dependencies::Available(reqs) = &vd.merged {
            Some(reqs.iter().cloned().collect::<Vec<_>>())
        } else {
            None
        }
    }

    /// Resolve a set of target packages using PubGrub.
    ///
    /// Creates an `__internal__/root` node whose dependencies are the given
    /// `targets`, runs the solver, then strips all `__internal__/` bookkeeping
    /// nodes (root + any USE-flag decision nodes) before returning.
    /// Callers only receive real Gentoo packages.  See `package.rs` for the
    /// full description of the `__internal__/` convention.
    ///
    /// Each target is a `(PortagePackage, PortageVersionSet)` pair, e.g. the
    /// package `dev-libs/openssl` with the version set `>=3.0`.
    #[allow(clippy::result_large_err)]
    pub fn resolve_targets(
        &mut self,
        targets: Vec<(PortagePackage, PortageVersionSet)>,
    ) -> std::result::Result<
        SelectedDependencies<PortagePackage, Version>,
        pubgrub::PubGrubError<Self>,
    > {
        let root = PortagePackage::synthetic_root();
        let root_ver = Version::parse("0").unwrap();

        // Root targets have no gating flag; merged is derived from by_class.
        let targets_with_flag: Vec<(PortagePackage, PortageVersionSet, Option<Interned<DefaultInterner>>)> =
            targets.into_iter().map(|(p, vs)| (p, vs, None)).collect();
        let vd = VersionData::from_by_class(vec![targets_with_flag, vec![], vec![], vec![], vec![]]);
        let entry = self
            .packages
            .entry(root.clone())
            .or_insert_with(|| PackageData { versions: BTreeMap::new() });
        entry.versions.insert(root_ver.clone(), vd);

        let solution = pubgrub::resolve(self, root.clone(), root_ver)?;
        self.packages.remove(&root);

        // Post-solve: collect USE flag requirements for all packages.  Must run
        // before filtering virtuals because per-branch constraints live in
        // Choice/SlotChoice nodes.
        self.use_flag_requirements = self.compute_use_flag_requirements(&solution);

        Ok(solution
            .into_iter()
            .filter(|(p, _)| !p.is_virtual())
            .collect())
    }

    /// Returns true if the deps of `vd` transitively reach any installed CPN,
    /// descending up to `depth` levels through `__internal__/*` virtual packages.
    fn deps_reach_installed(&self, vd: &VersionData, depth: u8) -> bool {
        let Dependencies::Available(ref constraints) = vd.merged else {
            return false;
        };
        for (dep_pkg, _) in constraints.iter() {
            if dep_pkg.is_virtual() {
                if depth > 0
                    && let Some(dep_data) = self.packages.get(dep_pkg)
                {
                    for dep_vd in dep_data.versions.values() {
                        if self.deps_reach_installed(dep_vd, depth - 1) {
                            return true;
                        }
                    }
                }
            } else if self.installed_cpns.contains(dep_pkg.cpn()) {
                return true;
            }
        }
        false
    }
}


/// Evaluate a single USE dep given the dep's effective state and the parent's
/// flag state (for Conditional/Equal kinds).
///
/// Returns `Some(requires_enabled)` when the constraint fires and is violated,
/// `None` when it is satisfied or the condition does not apply.
fn eval_violated_use_dep(
    kind: UseDepKind,
    dep_effective_enabled: bool,
    parent_flag_enabled: bool,
) -> Option<bool> {
    match kind {
        UseDepKind::Enabled => {
            (!dep_effective_enabled).then_some(true)
        }
        UseDepKind::Disabled => {
            dep_effective_enabled.then_some(false)
        }
        // [flag?]: if parent has flag → dep must have flag
        UseDepKind::Conditional => {
            (parent_flag_enabled && !dep_effective_enabled).then_some(true)
        }
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
    fn compute_use_flag_requirements(
        &self,
        solution: &SelectedDependencies<PortagePackage, Version>,
    ) -> Vec<UseFlagRequirement> {
        // Accumulate per target: (version, enable_set, disable_set, requirers).
        let mut by_target: HashMap<
            PortagePackage,
            (Version,
             std::collections::BTreeSet<Interned<DefaultInterner>>,
             std::collections::BTreeSet<Interned<DefaultInterner>>,
             std::collections::BTreeSet<String>),
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
                                .or_else(|| {
                                    self.installed_iuse.get(target_pkg).map(Vec::as_slice)
                                })
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
                            let entry = by_target
                                .entry(target_pkg.clone())
                                .or_insert_with(|| {
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
                let Some(vd) = self.packages.get(&pkg).and_then(|d| d.versions.get(&new_ver)) else { continue };
                let udeps = &vd.use_deps;

                // The "parent" is the upgraded package itself.
                let parent_is_installed = self.installed.contains_key(&pkg);
                for constraint in udeps {
                    let (target_pkg, vs) = &constraint.target;
                    if target_pkg.is_virtual() { continue; }
                    let (target_ver, is_installed) =
                        if let Some((inst_ver, _)) = self.installed.get(target_pkg) {
                            if vs.contains(inst_ver) { (inst_ver, true) } else { continue }
                        } else if let Some(sol_ver) = solution.get(target_pkg) {
                            if vs.contains(sol_ver) { (sol_ver, false) } else { continue }
                        } else { continue };

                    for ud in &constraint.use_deps {
                        let parent_flag_enabled = if parent_is_installed {
                            self.installed_use.get(&pkg).map_or(false, |u| u.contains(&ud.flag))
                                || by_target.get(&pkg).map_or(false, |(_, e, _, _)| e.contains(&ud.flag))
                        } else {
                            self.effective_flag_new(&pkg, &new_ver, &ud.flag, None)
                        };

                        let dep_effective_enabled = if is_installed {
                            let active = self.installed_use.get(target_pkg).map(Vec::as_slice).unwrap_or(&[]);
                            let iuse = self.packages.get(target_pkg)
                                .and_then(|d| d.versions.get(target_ver))
                                .map(|vd| vd.iuse.as_slice())
                                .or_else(|| self.installed_iuse.get(target_pkg).map(Vec::as_slice))
                                .unwrap_or(&[]);
                            let in_iuse = iuse.contains(&ud.flag);
                            if in_iuse { active.contains(&ud.flag) }
                            else { matches!(ud.default, Some(UseDefault::Enabled)) }
                        } else {
                            self.effective_flag_new(target_pkg, target_ver, &ud.flag, ud.default)
                        };

                        if let Some(req_en) = eval_violated_use_dep(ud.kind, dep_effective_enabled, parent_flag_enabled) {
                            let entry = by_target.entry(target_pkg.clone()).or_insert_with(|| {
                                (target_ver.clone(), std::collections::BTreeSet::new(), std::collections::BTreeSet::new(), std::collections::BTreeSet::new())
                            });
                            if req_en { entry.1.insert(ud.flag); } else { entry.2.insert(ud.flag); }
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
            .map(|(pkg, (ver, enable, disable, requirers))| UseFlagRequirement {
                package: pkg.clone(),
                version: ver,
                upgrade_to: upgrade_to.remove(&pkg),
                required_enabled: enable.into_iter().collect(),
                required_disabled: disable.into_iter().collect(),
                required_by: requirers.into_iter().collect(),
            })
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

    fn use_dep_branch_satisfied(&self, udeps: &[convert::UseDepConstraint]) -> bool {
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
            let iuse = self
                .packages
                .get(target_pkg)
                .and_then(|d| d.versions.get(inst_ver))
                .map(|vd| vd.iuse.as_slice())
                .filter(|s| !s.is_empty()) // empty == absent (see compute_use_flag_requirements)
                .or_else(|| self.installed_iuse.get(target_pkg).map(Vec::as_slice))
                .unwrap_or(&[]);
            for ud in &constraint.use_deps {
                let in_iuse = iuse.contains(&ud.flag);
                // Desired final state: flag is active now OR the global config
                // wants it enabled AND the package supports it (flag is in IUSE).
                let dep_effective_enabled = if in_iuse {
                    active.contains(&ud.flag)
                        || self.use_config.get(&ud.flag) == UseFlagState::Enabled
                } else {
                    matches!(ud.default, Some(UseDefault::Enabled))
                };
                // Parent's flag state: use global config as approximation
                // (OR groups are almost always in newly-installed packages).
                let parent_flag_enabled =
                    self.use_config.get(&ud.flag) == UseFlagState::Enabled;
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

fn register_virtual_choices(
    packages: &mut HashMap<PortagePackage, PackageData>,
    choices: Vec<convert::VirtualChoice>,
) {
    for vc in choices {
        let entry = packages
            .entry(vc.package)
            .or_insert_with(|| PackageData { versions: BTreeMap::new() });
        for (ver, deps) in vc.versions {
            let vd = VersionData::from_by_class(vec![deps, vec![], vec![], vec![], vec![]]);
            entry.versions.insert(ver, vd);
        }
        // Attach per-branch USE dep constraints so choose_version() can evaluate
        // which OR-group branch is already satisfied without rebuilds.
        for (ver, udeps) in vc.branch_use_deps {
            if !udeps.is_empty()
                && let Some(vd) = entry.versions.get_mut(&ver)
            {
                vd.use_deps = udeps;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{InMemoryRepository, PackageDeps, PackageVersions};
    use portage_atom::interner::Interned;
    use portage_atom::{Cpn, Dep, DepEntry};

    fn empty_deps() -> PackageDeps {
        PackageDeps {
            depend: vec![],
            rdepend: vec![],
            bdepend: vec![],
            pdepend: vec![],
            idepend: vec![],
        }
    }

    fn make_simple_repo() -> InMemoryRepository {
        let mut repo = InMemoryRepository::new();

        let openssl_cpv = portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap();
        repo.add_version(openssl_cpv, Some(Interned::intern("0")), None, empty_deps());

        let openssl_cpv2 = portage_atom::Cpv::parse("dev-libs/openssl-3.1.0").unwrap();
        repo.add_version(
            openssl_cpv2,
            Some(Interned::intern("0")),
            None,
            empty_deps(),
        );

        let rust_cpv = portage_atom::Cpv::parse("dev-lang/rust-1.75.0").unwrap();
        repo.add_version(
            rust_cpv,
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                depend: DepEntry::parse(">=dev-libs/openssl-3.0.0").unwrap(),
                rdepend: DepEntry::parse(">=dev-libs/openssl-3.0.0").unwrap(),
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );

        repo
    }

    #[test]
    fn provider_constructs() {
        let repo = make_simple_repo();
        let config = UseConfig::new();
        let _provider = PortageDependencyProvider::new(repo, config, &[]);
    }

    #[test]
    fn choose_highest_version() {
        let repo = make_simple_repo();
        let config = UseConfig::new();
        let provider = PortageDependencyProvider::new(repo, config, &[]);
        let openssl = PortagePackage::slotted(
            portage_atom::Cpn::parse("dev-libs/openssl").unwrap(),
            Interned::intern("0"),
        );
        let version = provider
            .choose_version(&openssl, &PortageVersionSet::any())
            .unwrap();
        assert_eq!(version, Some(Version::parse("3.1.0").unwrap()));
    }

    #[test]
    fn resolve_simple() {
        let repo = make_simple_repo();
        let config = UseConfig::new();
        let provider = PortageDependencyProvider::new(repo, config, &[]);
        let root = PortagePackage::slotted(
            portage_atom::Cpn::parse("dev-lang/rust").unwrap(),
            Interned::intern("0"),
        );
        let result = pubgrub::resolve(&provider, root, Version::parse("1.75.0").unwrap());
        assert!(result.is_ok());
        let solution = result.unwrap();
        assert!(
            solution
                .get(&PortagePackage::slotted(
                    portage_atom::Cpn::parse("dev-libs/openssl").unwrap(),
                    Interned::intern("0"),
                ))
                .is_some()
        );
    }

    #[test]
    fn multi_slot_installs_both_when_required() {
        let mut repo = InMemoryRepository::new();

        repo.add_version(
            portage_atom::Cpv::parse("dev-lang/python-3.11.9").unwrap(),
            Some(Interned::intern("3.11")),
            None,
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-lang/python-3.12.4").unwrap(),
            Some(Interned::intern("3.12")),
            None,
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                depend: vec![
                    DepEntry::Atom(Dep::parse("dev-lang/python:3.11").unwrap()),
                    DepEntry::Atom(Dep::parse("dev-lang/python:3.12").unwrap()),
                ],
                rdepend: vec![],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );

        let provider = PortageDependencyProvider::new(repo, UseConfig::new(), &[]);
        let root =
            PortagePackage::slotted(Cpn::parse("app-misc/myapp").unwrap(), Interned::intern("0"));
        let result = pubgrub::resolve(&provider, root, Version::parse("1.0").unwrap());
        assert!(result.is_ok());
        let solution = result.unwrap();
        assert!(
            solution
                .get(&PortagePackage::slotted(
                    Cpn::parse("dev-lang/python").unwrap(),
                    Interned::intern("3.11"),
                ))
                .is_some(),
            "python:3.11 should be in solution"
        );
        assert!(
            solution
                .get(&PortagePackage::slotted(
                    Cpn::parse("dev-lang/python").unwrap(),
                    Interned::intern("3.12"),
                ))
                .is_some(),
            "python:3.12 should be in solution"
        );
    }

    #[test]
    fn resolve_slot_operator_equal() {
        let mut repo = InMemoryRepository::new();

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                depend: DepEntry::parse("dev-libs/openssl:=").unwrap(),
                rdepend: vec![],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );

        let provider = PortageDependencyProvider::new(repo, UseConfig::new(), &[]);
        let root =
            PortagePackage::slotted(Cpn::parse("app-misc/myapp").unwrap(), Interned::intern("0"));
        let result = pubgrub::resolve(&provider, root, Version::parse("1.0").unwrap());
        assert!(result.is_ok());
        let solution = result.unwrap();
        assert!(
            solution
                .get(&PortagePackage::slotted(
                    Cpn::parse("dev-libs/openssl").unwrap(),
                    Interned::intern("0"),
                ))
                .is_some()
        );
    }

    #[test]
    fn resolve_slot_operator_star() {
        let mut repo = InMemoryRepository::new();

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                depend: DepEntry::parse("dev-libs/openssl:*").unwrap(),
                rdepend: vec![],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );

        let provider = PortageDependencyProvider::new(repo, UseConfig::new(), &[]);
        let root =
            PortagePackage::slotted(Cpn::parse("app-misc/myapp").unwrap(), Interned::intern("0"));
        let result = pubgrub::resolve(&provider, root, Version::parse("1.0").unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn installed_favored_picks_installed_version() {
        let mut repo = InMemoryRepository::new();

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.1.0").unwrap(),
            None,
            None,
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: DepEntry::parse(">=dev-libs/openssl-3.0.0").unwrap(),
                rdepend: vec![],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );

        let mut provider = PortageDependencyProvider::new(repo, UseConfig::new(), &[]);
        let openssl = PortagePackage::unslotted(Cpn::parse("dev-libs/openssl").unwrap());
        provider.add_installed(InstalledPackage {
            package: openssl,
            version: Version::parse("3.0.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![],
            iuse: vec![],
        });

        let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
        let solution = provider
            .resolve_targets(vec![(myapp, PortageVersionSet::any())])
            .unwrap();
        assert_eq!(
            solution.get(&PortagePackage::unslotted(
                Cpn::parse("dev-libs/openssl").unwrap()
            )),
            Some(&Version::parse("3.0.0").unwrap()),
            "should pick favored installed version 3.0.0 over 3.1.0"
        );
    }

    #[test]
    fn installed_favored_upgrades_when_required() {
        let mut repo = InMemoryRepository::new();

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.1.0").unwrap(),
            None,
            None,
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: DepEntry::parse(">=dev-libs/openssl-3.1.0").unwrap(),
                rdepend: vec![],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );

        let mut provider = PortageDependencyProvider::new(repo, UseConfig::new(), &[]);
        let openssl = PortagePackage::unslotted(Cpn::parse("dev-libs/openssl").unwrap());
        provider.add_installed(InstalledPackage {
            package: openssl,
            version: Version::parse("3.0.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![],
            iuse: vec![],
        });

        let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
        let solution = provider
            .resolve_targets(vec![(myapp, PortageVersionSet::any())])
            .unwrap();
        assert_eq!(
            solution.get(&PortagePackage::unslotted(
                Cpn::parse("dev-libs/openssl").unwrap()
            )),
            Some(&Version::parse("3.1.0").unwrap()),
            "should upgrade from favored 3.0.0 to 3.1.0 when required"
        );
    }

    #[test]
    fn installed_locked_pins_version() {
        let mut repo = InMemoryRepository::new();

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.0.0").unwrap(),
            None,
            None,
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/openssl-3.1.0").unwrap(),
            None,
            None,
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/myapp-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: DepEntry::parse(">=dev-libs/openssl-3.0.0").unwrap(),
                rdepend: vec![],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );

        let mut provider = PortageDependencyProvider::new(repo, UseConfig::new(), &[]);
        let openssl = PortagePackage::unslotted(Cpn::parse("dev-libs/openssl").unwrap());
        provider.add_installed(InstalledPackage {
            package: openssl,
            version: Version::parse("3.0.0").unwrap(),
            policy: InstalledPolicy::Lock,
            active_use: vec![],
            iuse: vec![],
        });

        let myapp = PortagePackage::unslotted(Cpn::parse("app-misc/myapp").unwrap());
        let solution = provider
            .resolve_targets(vec![(myapp, PortageVersionSet::any())])
            .unwrap();
        assert_eq!(
            solution.get(&PortagePackage::unslotted(
                Cpn::parse("dev-libs/openssl").unwrap()
            )),
            Some(&Version::parse("3.0.0").unwrap()),
            "locked should pin to 3.0.0 even though 3.1.0 exists"
        );
    }

    #[test]
    fn or_group_prefers_installed_alternative() {
        // || ( dev-libs/not-installed dev-libs/installed ) — installed is listed second.
        // Without installed preference the solver picks "not-installed" (higher choice version).
        // With installed preference it should pick "installed".
        let mut repo = InMemoryRepository::new();

        let not_inst = portage_atom::Cpv::parse("dev-libs/not-installed-1.0").unwrap();
        repo.add_version(not_inst, Some(Interned::intern("0")), None, empty_deps());

        let inst = portage_atom::Cpv::parse("dev-libs/installed-1.0").unwrap();
        repo.add_version(
            inst.clone(),
            Some(Interned::intern("0")),
            None,
            empty_deps(),
        );

        let consumer = portage_atom::Cpv::parse("app-misc/consumer-1.0").unwrap();
        repo.add_version(
            consumer,
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                depend: DepEntry::parse("|| ( dev-libs/not-installed dev-libs/installed )")
                    .unwrap(),
                rdepend: vec![],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );

        let config = UseConfig::new();
        let mut provider = PortageDependencyProvider::new(repo, config, &[]);

        provider.add_installed(InstalledPackage {
            package: PortagePackage::slotted(
                Cpn::parse("dev-libs/installed").unwrap(),
                Interned::intern("0"),
            ),
            version: Version::parse("1.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![],
            iuse: vec![],
        });

        let consumer_pkg = PortagePackage::slotted(
            Cpn::parse("app-misc/consumer").unwrap(),
            Interned::intern("0"),
        );
        let solution = provider
            .resolve_targets(vec![(consumer_pkg, PortageVersionSet::any())])
            .unwrap();

        let in_solution = |cpn: &str| {
            let pkg = PortagePackage::slotted(Cpn::parse(cpn).unwrap(), Interned::intern("0"));
            solution.get(&pkg).is_some()
        };

        assert!(
            in_solution("dev-libs/installed"),
            "installed package should be chosen"
        );
        assert!(
            !in_solution("dev-libs/not-installed"),
            "non-installed alternative should not be chosen"
        );
    }

    #[test]
    fn or_group_no_preference_when_both_installed() {
        // || ( A B ) where both A and B are installed — solver falls through to
        // highest choice version (A, listed first), same as without installed preference.
        let mut repo = InMemoryRepository::new();

        for cpv in ["dev-libs/a-1.0", "dev-libs/b-1.0"] {
            repo.add_version(
                portage_atom::Cpv::parse(cpv).unwrap(),
                Some(Interned::intern("0")),
                None,
                empty_deps(),
            );
        }

        let consumer = portage_atom::Cpv::parse("app-misc/consumer-1.0").unwrap();
        repo.add_version(
            consumer,
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                depend: DepEntry::parse("|| ( dev-libs/a dev-libs/b )").unwrap(),
                rdepend: vec![],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );

        let config = UseConfig::new();
        let mut provider = PortageDependencyProvider::new(repo, config, &[]);

        for cpn in ["dev-libs/a", "dev-libs/b"] {
            provider.add_installed(InstalledPackage {
                package: PortagePackage::slotted(Cpn::parse(cpn).unwrap(), Interned::intern("0")),
                version: Version::parse("1.0").unwrap(),
                policy: InstalledPolicy::Favor,
                active_use: vec![],
                iuse: vec![],
            });
        }

        let consumer_pkg = PortagePackage::slotted(
            Cpn::parse("app-misc/consumer").unwrap(),
            Interned::intern("0"),
        );
        let solution = provider
            .resolve_targets(vec![(consumer_pkg, PortageVersionSet::any())])
            .unwrap();

        // With both installed, falls through to highest choice version = a (listed first).
        let in_sol = |cpn: &str| {
            solution
                .get(&PortagePackage::slotted(
                    Cpn::parse(cpn).unwrap(),
                    Interned::intern("0"),
                ))
                .is_some()
        };
        assert!(
            in_sol("dev-libs/a"),
            "first alternative chosen when both installed"
        );
        assert!(!in_sol("dev-libs/b"));
    }

    #[test]
    fn or_group_prefers_installed_with_slot_nesting() {
        // Mirrors the real-world case: || ( >=A-1.0:* >=B-1.0:* ) where A has
        // multiple slots (triggering the choice→slot→pkg two-level nesting) and
        // only B is installed.  The solver should pick B.
        let mut repo = InMemoryRepository::new();

        // A has two slots (1.0 and 2.0) — not installed
        for (cpv, slot) in [("dev-libs/a-1.0", "1"), ("dev-libs/a-2.0", "2")] {
            repo.add_version(
                portage_atom::Cpv::parse(cpv).unwrap(),
                Some(Interned::intern(slot)),
                None,
                empty_deps(),
            );
        }

        // B has a single slot — installed
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            empty_deps(),
        );

        let consumer = portage_atom::Cpv::parse("app-misc/consumer-1.0").unwrap();
        repo.add_version(
            consumer,
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                // slot-star deps trigger push_unslotted_or_choice → slot_* nesting
                depend: DepEntry::parse("|| ( dev-libs/a:* dev-libs/b:* )").unwrap(),
                rdepend: vec![],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );

        let config = UseConfig::new();
        let mut provider = PortageDependencyProvider::new(repo, config, &[]);

        provider.add_installed(InstalledPackage {
            package: PortagePackage::slotted(
                Cpn::parse("dev-libs/b").unwrap(),
                Interned::intern("0"),
            ),
            version: Version::parse("1.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![],
            iuse: vec![],
        });

        let consumer_pkg = PortagePackage::slotted(
            Cpn::parse("app-misc/consumer").unwrap(),
            Interned::intern("0"),
        );
        let solution = provider
            .resolve_targets(vec![(consumer_pkg, PortageVersionSet::any())])
            .unwrap();

        let b_in_sol = solution
            .get(&PortagePackage::slotted(
                Cpn::parse("dev-libs/b").unwrap(),
                Interned::intern("0"),
            ))
            .is_some();
        let a_in_sol = solution
            .iter()
            .any(|(p, _)| p.cpn().package.as_str() == "a");

        assert!(
            b_in_sol,
            "installed B should be chosen over non-installed A"
        );
        assert!(!a_in_sol, "non-installed A should not appear in solution");
    }

    #[test]
    fn or_group_prefers_branch_satisfying_use_deps() {
        // Mirrors the librsvg BDEPEND case:
        //   || ( ( python:3.14  docutils[python_targets_python3_14(-)] )
        //        ( python:3.13  docutils[python_targets_python3_13(-)] ) )
        // Both python slots are installed.  docutils has python_targets_python3_13
        // enabled but python_targets_python3_14 disabled.
        // Expected: solver picks branch 2 (python:3.13) since its USE dep is satisfied.
        let mut repo = InMemoryRepository::new();

        // python:3.14 — installed
        repo.add_version(
            portage_atom::Cpv::parse("dev-lang/python-3.14.0").unwrap(),
            Some(Interned::intern("3.14")),
            None,
            empty_deps(),
        );
        // python:3.13 — installed
        repo.add_version(
            portage_atom::Cpv::parse("dev-lang/python-3.13.0").unwrap(),
            Some(Interned::intern("3.13")),
            None,
            empty_deps(),
        );
        // docutils — has both python_targets flags in IUSE, only p3.13 enabled
        repo.add_version_with_iuse(
            portage_atom::Cpv::parse("dev-python/docutils-0.21.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            vec![
                Interned::intern("python_targets_python3_13"),
                Interned::intern("python_targets_python3_14"),
            ],
            empty_deps(),
        );

        // consumer has the OR group via an AllOf pair (simplified encoding)
        repo.add_version(
            portage_atom::Cpv::parse("media-libs/librsvg-2.60.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                bdepend: DepEntry::parse(
                    "|| ( \
                       ( dev-lang/python:3.14 dev-python/docutils[python_targets_python3_14(-)] ) \
                       ( dev-lang/python:3.13 dev-python/docutils[python_targets_python3_13(-)] ) \
                     )",
                )
                .unwrap(),
                depend: vec![],
                rdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );

        let config = UseConfig::new();
        let mut provider = PortageDependencyProvider::new(repo, config, &[]);

        // Install python:3.14, python:3.13, and docutils with p3.13 active
        provider.add_installed(InstalledPackage {
            package: PortagePackage::slotted(
                Cpn::parse("dev-lang/python").unwrap(),
                Interned::intern("3.14"),
            ),
            version: Version::parse("3.14.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![],
            iuse: vec![],
        });
        provider.add_installed(InstalledPackage {
            package: PortagePackage::slotted(
                Cpn::parse("dev-lang/python").unwrap(),
                Interned::intern("3.13"),
            ),
            version: Version::parse("3.13.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![],
            iuse: vec![],
        });
        provider.add_installed(InstalledPackage {
            package: PortagePackage::slotted(
                Cpn::parse("dev-python/docutils").unwrap(),
                Interned::intern("0"),
            ),
            version: Version::parse("0.21.0").unwrap(),
            policy: InstalledPolicy::Favor,
            // Only python3_13 is enabled; python3_14 is in IUSE but disabled
            active_use: vec![Interned::intern("python_targets_python3_13")],
            iuse: vec![
                Interned::intern("python_targets_python3_13"),
                Interned::intern("python_targets_python3_14"),
            ],
        });

        let librsvg = PortagePackage::slotted(
            Cpn::parse("media-libs/librsvg").unwrap(),
            Interned::intern("0"),
        );
        let solution = provider
            .resolve_targets(vec![(librsvg, PortageVersionSet::any())])
            .unwrap();

        let has = |pkg: &str, slot: &str| {
            solution
                .get(&PortagePackage::slotted(
                    Cpn::parse(pkg).unwrap(),
                    Interned::intern(slot),
                ))
                .is_some()
        };

        assert!(
            has("dev-lang/python", "3.13"),
            "branch 2 (python:3.13) should be chosen since docutils p3.13 USE dep is satisfied"
        );
        assert!(
            !has("dev-lang/python", "3.14"),
            "branch 1 (python:3.14) should not be chosen — docutils p3.14 USE dep is NOT satisfied"
        );
    }

    #[test]
    fn reinstall_deps_detected_for_direct_use_dep_violation() {
        // Package A (newly installed) has a direct RDEPEND on B[flag].
        // B is already installed but with flag disabled → B must be rebuilt (R).
        let mut repo = InMemoryRepository::new();

        repo.add_version_with_iuse(
            portage_atom::Cpv::parse("dev-python/b-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            vec![Interned::intern("flag")],
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                rdepend: DepEntry::parse("dev-python/b[flag]").unwrap(),
                depend: vec![],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );

        let config = UseConfig::new();
        let mut provider = PortageDependencyProvider::new(repo, config, &[]);

        // B is installed but flag is disabled
        provider.add_installed(InstalledPackage {
            package: PortagePackage::slotted(
                Cpn::parse("dev-python/b").unwrap(),
                Interned::intern("0"),
            ),
            version: Version::parse("1.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![],  // flag NOT active
            iuse: vec![Interned::intern("flag")],
        });

        let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
        provider
            .resolve_targets(vec![(a, PortageVersionSet::any())])
            .unwrap();

        let reinstall = provider.reinstall_deps();
        assert_eq!(reinstall.len(), 1, "B must be flagged for reinstall");
        assert_eq!(reinstall[0].package.cpn().package.as_str(), "b");
    }

    #[test]
    fn reinstall_deps_empty_when_use_dep_satisfied() {
        // Same setup as above, but B is installed with flag enabled → no reinstall.
        let mut repo = InMemoryRepository::new();

        repo.add_version_with_iuse(
            portage_atom::Cpv::parse("dev-python/b-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            vec![Interned::intern("flag")],
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                rdepend: DepEntry::parse("dev-python/b[flag]").unwrap(),
                depend: vec![],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );

        let config = UseConfig::new();
        let mut provider = PortageDependencyProvider::new(repo, config, &[]);

        provider.add_installed(InstalledPackage {
            package: PortagePackage::slotted(
                Cpn::parse("dev-python/b").unwrap(),
                Interned::intern("0"),
            ),
            version: Version::parse("1.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![Interned::intern("flag")],  // flag IS active
            iuse: vec![Interned::intern("flag")],
        });

        let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
        provider
            .resolve_targets(vec![(a, PortageVersionSet::any())])
            .unwrap();

        assert!(
            provider.reinstall_deps().is_empty(),
            "no reinstall needed when USE dep is already satisfied"
        );
    }

    // ---- Characterization: autounmask "needed" USE detection ----
    //
    // These pin the observable behaviour that the `desired_use` concern
    // extraction (step 3) must preserve: a flag is reported as needed only when
    // it is NOT already provided — whether "provided" comes from the ebuild's
    // IUSE default or from the global USE config.  When step 3 moves policy
    // resolution behind `PackageRepository::desired_use`, the *setup* here will
    // change (the caller will fold IUSE defaults / config into the desired set),
    // but the assertions — needed vs not-needed — must stay identical.

    /// `a` RDEPENDs `b[flag]`; `flag` is off everywhere → `b` needs it enabled.
    #[test]
    fn use_flag_needed_when_flag_off() {
        let mut repo = InMemoryRepository::new();
        repo.add_version_with_iuse(
            portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            vec![Interned::intern("flag")],
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                rdepend: DepEntry::parse("dev-libs/b[flag]").unwrap(),
                ..empty_deps()
            },
        );
        let mut provider = PortageDependencyProvider::new(repo, UseConfig::new(), &[]);
        let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
        provider
            .resolve_targets(vec![(a, PortageVersionSet::any())])
            .unwrap();

        let b = provider
            .use_flag_requirements()
            .iter()
            .find(|r| r.package.cpn().package.as_str() == "b")
            .expect("b should have a USE requirement");
        assert!(b.required_enabled.contains(&Interned::intern("flag")));
    }

    /// Same, but `b` carries `+flag` as an IUSE default → already on, none needed.
    #[test]
    fn use_flag_not_needed_when_iuse_default_on() {
        let mut repo = InMemoryRepository::new();
        let mut defaults = HashMap::new();
        defaults.insert(Interned::intern("flag"), IUseDefault::Enabled);
        repo.add_package_versions(
            portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
            PackageVersions {
                slot: Some(Interned::intern("0")),
                subslot: None,
                repo: None,
                iuse: vec![Interned::intern("flag")],
                iuse_defaults: defaults,
                deps: empty_deps(),
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                rdepend: DepEntry::parse("dev-libs/b[flag]").unwrap(),
                ..empty_deps()
            },
        );
        let mut provider = PortageDependencyProvider::new(repo, UseConfig::new(), &[]);
        let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
        provider
            .resolve_targets(vec![(a, PortageVersionSet::any())])
            .unwrap();

        assert!(
            provider
                .use_flag_requirements()
                .iter()
                .all(|r| r.required_enabled.is_empty()),
            "IUSE +flag default already satisfies b[flag]; no autounmask needed"
        );
    }

    /// Same, but the global config already enables `flag` → none needed.
    #[test]
    fn use_flag_not_needed_when_config_enables() {
        let mut repo = InMemoryRepository::new();
        repo.add_version_with_iuse(
            portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            vec![Interned::intern("flag")],
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                rdepend: DepEntry::parse("dev-libs/b[flag]").unwrap(),
                ..empty_deps()
            },
        );
        let mut config = UseConfig::new();
        config.enable(Interned::intern("flag"));
        let mut provider = PortageDependencyProvider::new(repo, config, &[]);
        let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
        provider
            .resolve_targets(vec![(a, PortageVersionSet::any())])
            .unwrap();

        assert!(
            provider
                .use_flag_requirements()
                .iter()
                .all(|r| r.required_enabled.is_empty()),
            "global config already enables flag; no autounmask needed"
        );
    }

    #[test]
    fn packages_for_cpn_excludes_virtual_choice_nodes() {
        // Multi-slot packages cause register_virtual_choices to insert Choice
        // nodes into self.packages. packages_for_cpn must skip those nodes
        // rather than calling cpn() on them (which panics).
        let mut repo = InMemoryRepository::new();
        repo.add_version(
            portage_atom::Cpv::parse("dev-lang/python-3.11.9").unwrap(),
            Some(Interned::intern("3.11")),
            None,
            empty_deps(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-lang/python-3.12.4").unwrap(),
            Some(Interned::intern("3.12")),
            None,
            empty_deps(),
        );

        let provider = PortageDependencyProvider::new(repo, UseConfig::new(), &[]);
        let cpn = Cpn::parse("dev-lang/python").unwrap();
        let pkgs = provider.packages_for_cpn(&cpn);

        assert_eq!(pkgs.len(), 2, "expected one entry per slot");
        assert!(pkgs.iter().all(|p| !p.is_virtual()), "no virtual nodes");
        assert!(
            pkgs.iter()
                .any(|p| p.slot() == Some(Interned::intern("3.11"))),
            "slot 3.11 present"
        );
        assert!(
            pkgs.iter()
                .any(|p| p.slot() == Some(Interned::intern("3.12"))),
            "slot 3.12 present"
        );
    }
}
