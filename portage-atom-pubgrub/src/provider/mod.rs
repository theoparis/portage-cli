use std::collections::{BTreeMap, HashMap, HashSet};

use crate::repository::IUseDefault;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Dep, Version};
use pubgrub::{Dependencies, SelectedDependencies};

use crate::convert;
use crate::package::PortagePackage;
use crate::repository::PackageRepository;
use crate::use_config::{UseConfig, UseFlagState};
use crate::version_set::PortageVersionSet;

/// Post-solve USE-requirement analysis.
mod post_solve;
/// The PubGrub `DependencyProvider` impl (prioritise / choose_version /
/// get_dependencies).
mod solve;

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
    pub(crate) by_class: Vec<Vec<convert::Req>>,
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
    fn from_by_class(by_class: Vec<Vec<convert::Req>>) -> Self {
        let merged = Dependencies::Available(
            by_class
                .iter()
                .flatten()
                .map(|(p, vs, _)| (p.clone(), vs.clone()))
                .collect(),
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
    /// USE flag requirements collected by the post-solve validation pass.
    ///
    /// Covers both reinstall cases (`R`: installed packages with violated
    /// constraints) and informational cases (`N`/`U`: new packages whose
    /// required flags may not be set by the current global config).
    pub(crate) use_flag_requirements: Vec<UseFlagRequirement>,
    /// Installed packages that a previous solve iteration decided to upgrade to a
    /// newer repo version (`upgrade_to`).  On the next iteration the solver pins
    /// these to the new version so its full dependency closure is re-solved,
    /// instead of leaving the upgraded version's deps unaccounted for.  Cleared
    /// at the start of every [`resolve_targets`](Self::resolve_targets) call.
    pub(crate) upgrade_pins: HashMap<PortagePackage, Version>,
    /// Explicitly requested target packages (set by `resolve_targets`).
    /// `choose_version` does not favor the installed version for these:
    /// a named argument selects the best accepted version, as emerge does
    /// (installed-and-best still resolves to the installed version).
    pub(crate) root_targets: std::collections::HashSet<PortagePackage>,
    /// Preferred version (`0`/`1`) for each `UseDecision` node, i.e. the value
    /// the caller's policy would have given the ceded flag.  `choose_version`
    /// biases toward it so a `SolverDecided` flag only flips when a constraint
    /// forces it (greedy keep-configured — see `docs/required-use-level-c.md`).
    pub(crate) use_decision_prefer: HashMap<PortagePackage, Version>,
    /// Reverse map from a `UseDecision` node to the `(cpn, flag)` it decides,
    /// so the chosen values can be reported back to the caller by name.
    pub(crate) use_decision_meta: HashMap<PortagePackage, (Cpn, Interned<DefaultInterner>)>,
    /// The value the solver chose for each `UseDecision` node in the last solve
    /// (`true` = on). Captured before virtual nodes are stripped from the result.
    pub(crate) solved_use_decisions: HashMap<PortagePackage, bool>,
}

/// A USE flag the caller ceded to the solver, with the value the solver chose.
#[derive(Debug, Clone)]
pub struct CededFlag {
    /// The package the flag belongs to.
    pub cpn: Cpn,
    /// The ceded flag.
    pub flag: Interned<DefaultInterner>,
    /// The value the solver chose (`true` = enabled).
    pub value: bool,
    /// `true` when the chosen value differs from the caller's preference, i.e.
    /// the solver flipped it to satisfy a constraint.
    pub flipped: bool,
}

impl PortageDependencyProvider {
    /// Build the provider from a repository.
    ///
    /// All USE policy is the repository's concern: each version's effective
    /// desired USE is obtained via [`PackageRepository::desired_use`] (which
    /// folds global USE, `package.use`, and IUSE defaults).  The solver never
    /// resolves policy itself.  See `docs/use-and-solver-boundary.md`.
    pub fn new<R: PackageRepository>(repo: R) -> Self {
        let seeds = repo.all_packages();
        Self::new_with_seeds(repo, seeds)
    }

    /// Like [`new`](Self::new), but converts only the packages *reachable*
    /// from `seeds` (typically the resolve targets plus the installed set).
    /// References are followed transitively, so after ingestion a referenced
    /// package missing from `packages` is genuinely absent from the
    /// repository — the dropped-dependency filtering stays sound. For a
    /// targeted resolve this converts a few hundred packages instead of the
    /// whole tree.
    pub fn new_for_targets<R: PackageRepository>(repo: R, seeds: Vec<Cpn>) -> Self {
        Self::new_with_seeds(repo, seeds)
    }

    fn new_with_seeds<R: PackageRepository>(repo: R, seeds: Vec<Cpn>) -> Self {
        let mut packages = HashMap::new();
        let mut use_decision_prefer: HashMap<PortagePackage, Version> = HashMap::new();
        let mut use_decision_meta: HashMap<PortagePackage, (Cpn, Interned<DefaultInterner>)> =
            HashMap::new();
        let mut cpn_slots: HashMap<portage_atom::Cpn, Vec<Interned<DefaultInterner>>> =
            HashMap::new();

        // First pass: collect slots per CPN via the cheap `slots_for`
        // projection (same version filters as `versions_for`, no dep
        // conversion). The slot map must cover the whole repository so
        // unslotted deps on multi-slot packages resolve no matter which
        // package references them.
        for cpn in repo.all_packages() {
            let slots = repo.slots_for(&cpn);
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

        // Second pass: register versions and convert deps for the closure of
        // `seeds` — every package referenced by a converted dependency (or a
        // virtual choice branch) is queued in turn.
        let mut queue: std::collections::VecDeque<Cpn> = seeds.into_iter().collect();
        let mut seen: HashSet<Cpn> = queue.iter().copied().collect();
        while let Some(cpn) = queue.pop_front() {
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

                // The resolved desired USE for this version (caller's policy:
                // package.use + global USE folded over IUSE defaults).  This is
                // the single authoritative set every later reader consults.
                let cpv_use_cfg = repo.desired_use(&cpv);

                // Record the preferred value of every ceded (`SolverDecided`)
                // flag so `choose_version` can bias its `UseDecision` node toward
                // the caller's configured value (greedy keep-configured).
                for flag in cpv_use_cfg.solver_decided_flags() {
                    if let UseFlagState::SolverDecided { prefer } = cpv_use_cfg.get(&flag) {
                        let node = convert::use_decision_package(&cpn_str, &flag);
                        let ver = Version::new(&[u64::from(prefer)]);
                        use_decision_prefer.insert(node.clone(), ver);
                        use_decision_meta.insert(node, (cpn, flag));
                    }
                }

                let class_results: [convert::ConversionResult; 5] = dep_classes.map(|entries| {
                    convert::convert_deps(entries, &cpn_str, &cpv_use_cfg, &slot_map)
                });

                let mut all_blockers = Vec::new();
                let mut all_use_deps = Vec::new();
                let mut all_repo_constraints = Vec::new();
                let mut all_virtual_choices = Vec::new();
                let mut all_slot_operator_deps = Vec::new();
                let mut by_class: Vec<Vec<convert::Req>> = Vec::with_capacity(5);

                for result in class_results {
                    all_blockers.extend(result.blockers);
                    all_use_deps.extend(result.use_deps);
                    all_repo_constraints.extend(result.repo_constraints);
                    all_virtual_choices.extend(result.virtual_choices);
                    all_slot_operator_deps.extend(result.slot_operator_deps);
                    by_class.push(result.requirements);
                }

                // Level-C: encode REQUIRED_USE over the package's UseDecision
                // nodes (only ceded flags produce constraints; with everything
                // fixed this is inert). The pull/force/choice requirements ride
                // in the DEPEND class (index 0); they reference virtual nodes,
                // which are stripped from the install order.
                if let Some(ru) = &meta.required_use {
                    let enc = convert::encode_required_use(&cpn_str, ru, &cpv_use_cfg);
                    by_class[0].extend(enc.requirements);
                    all_virtual_choices.extend(enc.virtual_choices);
                }

                // Queue every real package this version references (direct
                // requirements and virtual-choice branches) for ingestion.
                for class in &by_class {
                    for (target, _, _) in class {
                        if !target.is_virtual() {
                            let c = *target.cpn();
                            if seen.insert(c) {
                                queue.push_back(c);
                            }
                        }
                    }
                }
                for vc in &all_virtual_choices {
                    for (_, deps) in &vc.versions {
                        for (target, _, _) in deps {
                            if !target.is_virtual() {
                                let c = *target.cpn();
                                if seen.insert(c) {
                                    queue.push_back(c);
                                }
                            }
                        }
                    }
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

                let entry = packages.entry(pkg).or_insert_with(|| PackageData {
                    versions: BTreeMap::new(),
                });
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
                        DroppedDep {
                            package: pkg,
                            version_set: vs,
                            alternatives,
                        }
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
            use_flag_requirements: Vec::new(),
            upgrade_pins: HashMap::new(),
            root_targets: std::collections::HashSet::new(),
            use_decision_prefer,
            use_decision_meta,
            solved_use_decisions: HashMap::new(),
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
        if let Some(pkg_data) = self.packages.get_mut(&installed.package)
            && !pkg_data.versions.contains_key(&installed.version)
        {
            let vd = VersionData::from_by_class(vec![vec![], vec![], vec![], vec![], vec![]]);
            pkg_data.versions.insert(installed.version.clone(), vd);
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

        self.root_targets = targets.iter().map(|(p, _)| p.clone()).collect();

        // Root targets have no gating flag; merged is derived from by_class.
        let targets_with_flag: Vec<(
            PortagePackage,
            PortageVersionSet,
            Option<Interned<DefaultInterner>>,
        )> = targets.into_iter().map(|(p, vs)| (p, vs, None)).collect();
        let vd =
            VersionData::from_by_class(vec![targets_with_flag, vec![], vec![], vec![], vec![]]);
        let entry = self
            .packages
            .entry(root.clone())
            .or_insert_with(|| PackageData {
                versions: BTreeMap::new(),
            });
        entry.versions.insert(root_ver.clone(), vd);

        // Re-solve to a fixpoint so that any installed package the post-solve
        // pass decides to upgrade (`upgrade_to`) has its *new* version's full
        // dependency closure solved, not just the installed version's runtime
        // deps.  Each iteration pins the upgrades discovered so far (see
        // `upgrade_pins` / `choose_version`) and solves again; new upgrades
        // surfaced by the richer graph feed the next round.  Bounded so a
        // pathological oscillation can't loop forever — on the rare re-solve
        // failure or the bound, we keep the last good solution (the previous
        // sound approximation).
        self.upgrade_pins.clear();
        const MAX_RESOLVE_ITERS: usize = 4;
        let mut solution = pubgrub::resolve(self, root.clone(), root_ver.clone())?;
        // Post-solve: collect USE flag requirements for all packages.  Must run
        // before filtering virtuals because per-branch constraints live in
        // Choice/SlotChoice nodes.
        self.use_flag_requirements = self.compute_use_flag_requirements(&solution);

        for _ in 1..MAX_RESOLVE_ITERS {
            // Pin every upgrade discovered so far; stop once nothing new appears.
            let mut added_pin = false;
            let pins: Vec<(PortagePackage, Version)> = self
                .use_flag_requirements
                .iter()
                .filter_map(|r| r.upgrade_to.clone().map(|v| (r.package.clone(), v)))
                .collect();
            for (pkg, ver) in pins {
                if self.upgrade_pins.get(&pkg) != Some(&ver) {
                    self.upgrade_pins.insert(pkg, ver);
                    added_pin = true;
                }
            }
            if !added_pin {
                break;
            }

            // Re-solve with the new pins.  If it fails (e.g. the upgraded
            // version's deps can't be satisfied), keep the last good solution
            // rather than turning an advisory situation into a hard error.
            match pubgrub::resolve(self, root.clone(), root_ver.clone()) {
                Ok(next) => {
                    solution = next;
                    self.use_flag_requirements = self.compute_use_flag_requirements(&solution);
                }
                Err(_) => break,
            }
        }

        self.packages.remove(&root);

        // Capture the solver's choice for every ceded flag before the virtual
        // UseDecision nodes are stripped from the returned solution.
        self.solved_use_decisions = solution
            .iter()
            .filter(|(p, _)| matches!(p, PortagePackage::UseDecision { .. }))
            .map(|(p, v)| (p.clone(), *v == Version::new(&[1])))
            .collect();

        Ok(solution
            .into_iter()
            .filter(|(p, _)| !p.is_virtual())
            .collect())
    }

    /// The flags the caller ceded to the solver, with the values it chose.
    ///
    /// Empty unless the caller emitted `SolverDecided` flags (Level-C). Lets the
    /// caller fold the chosen values back into its effective-USE display and
    /// report any the solver flipped. See `docs/required-use-level-c.md`.
    pub fn solved_use_decisions(&self) -> Vec<CededFlag> {
        let mut out: Vec<CededFlag> = self
            .solved_use_decisions
            .iter()
            .filter_map(|(node, &value)| {
                let (cpn, flag) = self.use_decision_meta.get(node)?;
                let preferred = self
                    .use_decision_prefer
                    .get(node)
                    .map(|v| *v == Version::new(&[1]));
                Some(CededFlag {
                    cpn: *cpn,
                    flag: *flag,
                    value,
                    flipped: preferred != Some(value),
                })
            })
            .collect();
        out.sort_by(|a, b| {
            a.cpn
                .cmp(&b.cpn)
                .then_with(|| a.flag.as_str().cmp(b.flag.as_str()))
        });
        out
    }

    /// Returns true if the deps of `vd` transitively reach any installed CPN,
    /// descending up to `depth` levels through `__internal__/*` virtual packages.
    pub(crate) fn deps_reach_installed(&self, vd: &VersionData, depth: u8) -> bool {
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
fn register_virtual_choices(
    packages: &mut HashMap<PortagePackage, PackageData>,
    choices: Vec<convert::VirtualChoice>,
) {
    for vc in choices {
        let entry = packages.entry(vc.package).or_insert_with(|| PackageData {
            versions: BTreeMap::new(),
        });
        for (ver, deps) in vc.versions {
            // Merge, don't overwrite: a UseDecision node can be produced by both
            // the conditional-dep path and the REQUIRED_USE encoder for the same
            // (cpn, flag). Selecting a version must enforce *all* of its deps.
            match entry.versions.get_mut(&ver) {
                Some(existing) => {
                    existing.by_class[0].extend(deps);
                    existing.merged = Dependencies::Available(
                        existing
                            .by_class
                            .iter()
                            .flatten()
                            .map(|(p, vs, _)| (p.clone(), vs.clone()))
                            .collect(),
                    );
                }
                None => {
                    let vd = VersionData::from_by_class(vec![deps, vec![], vec![], vec![], vec![]]);
                    entry.versions.insert(ver, vd);
                }
            }
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
    use pubgrub::DependencyProvider as _; // for choose_version in tests

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
        let mut repo = make_simple_repo();
        let config = UseConfig::new();
        let _provider = {
            repo.set_use_config(config);
            PortageDependencyProvider::new(repo)
        };
    }

    #[test]
    fn choose_highest_version() {
        let mut repo = make_simple_repo();
        let config = UseConfig::new();
        let provider = {
            repo.set_use_config(config);
            PortageDependencyProvider::new(repo)
        };
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
        let mut repo = make_simple_repo();
        let config = UseConfig::new();
        let provider = {
            repo.set_use_config(config);
            PortageDependencyProvider::new(repo)
        };
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

        let provider = PortageDependencyProvider::new(repo);
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

        let provider = PortageDependencyProvider::new(repo);
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

        let provider = PortageDependencyProvider::new(repo);
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

        let mut provider = PortageDependencyProvider::new(repo);
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

        let mut provider = PortageDependencyProvider::new(repo);
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

        let mut provider = PortageDependencyProvider::new(repo);
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
        let mut provider = {
            repo.set_use_config(config);
            PortageDependencyProvider::new(repo)
        };

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
        let mut provider = {
            repo.set_use_config(config);
            PortageDependencyProvider::new(repo)
        };

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
        let mut provider = {
            repo.set_use_config(config);
            PortageDependencyProvider::new(repo)
        };

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
        let mut provider = {
            repo.set_use_config(config);
            PortageDependencyProvider::new(repo)
        };

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
        let mut provider = {
            repo.set_use_config(config);
            PortageDependencyProvider::new(repo)
        };

        // B is installed but flag is disabled
        provider.add_installed(InstalledPackage {
            package: PortagePackage::slotted(
                Cpn::parse("dev-python/b").unwrap(),
                Interned::intern("0"),
            ),
            version: Version::parse("1.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![], // flag NOT active
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
        let mut provider = {
            repo.set_use_config(config);
            PortageDependencyProvider::new(repo)
        };

        provider.add_installed(InstalledPackage {
            package: PortagePackage::slotted(
                Cpn::parse("dev-python/b").unwrap(),
                Interned::intern("0"),
            ),
            version: Version::parse("1.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![Interned::intern("flag")], // flag IS active
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

    #[test]
    fn upgrade_to_resolves_new_versions_deps() {
        // Regression for the "post-solve remap does not re-solve" gap (#4):
        // when a forced rebuild of an installed package is favoured up to a
        // newer repo version, that newer version's dependency closure must be
        // part of the plan.
        //
        // Setup:
        //   - b-1.0 installed (flag off) — the installed version has NO deps.
        //   - b-2.0 in the tree RDEPENDs a brand-new package c (which b-1.0
        //     lacks).
        //   - a-1.0 RDEPENDs b[flag] → b must rebuild → upgrade to b-2.0.
        // Before the fix, the solve used b-1.0's (empty) deps and c never
        // appeared; after the fix the re-solve pins b-2.0 and pulls in c.
        let mut repo = InMemoryRepository::new();

        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/c-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            empty_deps(),
        );
        // Installed version: no deps.
        repo.add_version_with_iuse(
            portage_atom::Cpv::parse("dev-python/b-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            vec![Interned::intern("flag")],
            empty_deps(),
        );
        // Newer version: gains an RDEPEND on c.
        repo.add_version_with_iuse(
            portage_atom::Cpv::parse("dev-python/b-2.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            vec![Interned::intern("flag")],
            PackageDeps {
                rdepend: DepEntry::parse("dev-libs/c").unwrap(),
                ..empty_deps()
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                rdepend: DepEntry::parse("dev-python/b[flag]").unwrap(),
                ..empty_deps()
            },
        );

        let config = UseConfig::new();
        let mut provider = {
            repo.set_use_config(config);
            PortageDependencyProvider::new(repo)
        };

        // b is installed at 1.0 with flag disabled → rebuild forced by a.
        provider.add_installed(InstalledPackage {
            package: PortagePackage::slotted(
                Cpn::parse("dev-python/b").unwrap(),
                Interned::intern("0"),
            ),
            version: Version::parse("1.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![], // flag NOT active
            iuse: vec![Interned::intern("flag")],
        });

        let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
        let solution = provider
            .resolve_targets(vec![(a, PortageVersionSet::any())])
            .unwrap();

        // b must be upgraded to 2.0 ...
        let b_ver = solution
            .iter()
            .find(|(p, _)| p.cpn().package.as_str() == "b")
            .map(|(_, v)| v.clone());
        assert_eq!(
            b_ver,
            Some(Version::parse("2.0").unwrap()),
            "b should be upgraded to 2.0"
        );
        // ... and 2.0's new dependency c must be in the plan.
        assert!(
            solution
                .iter()
                .any(|(p, _)| p.cpn().package.as_str() == "c"),
            "c (a new dependency of b-2.0) must be pulled into the re-solved plan"
        );
    }

    #[test]
    fn required_use_of_fixed_flags_never_constrains_the_solve() {
        // With no flags ceded, the encoder partially evaluates REQUIRED_USE
        // against the fixed config and emits no constraints — violations are
        // Level A's domain (docs/required-use-level-c.md). Proven two ways:
        // (1) a package whose REQUIRED_USE is unsatisfiable still resolves (no
        //     NoSolution, same version), and
        // (2) the solution is byte-identical to the same repo without the fact.
        use crate::required_use::RequiredUse;

        let build = |with_ru: bool| {
            let mut repo = InMemoryRepository::new();
            let deps = PackageDeps {
                rdepend: DepEntry::parse("dev-libs/b").unwrap(),
                ..empty_deps()
            };
            // ^^ ( x y ) with both flags off by default → Level-A violation.
            let ru = RequiredUse::ExactlyOne(vec![
                RequiredUse::Flag {
                    name: Interned::intern("x"),
                    negated: false,
                },
                RequiredUse::Flag {
                    name: Interned::intern("y"),
                    negated: false,
                },
            ]);
            if with_ru {
                repo.add_version_with_required_use(
                    portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
                    Some(Interned::intern("0")),
                    vec![Interned::intern("x"), Interned::intern("y")],
                    deps,
                    ru,
                );
            } else {
                repo.add_version_with_iuse(
                    portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
                    Some(Interned::intern("0")),
                    None,
                    vec![Interned::intern("x"), Interned::intern("y")],
                    deps,
                );
            }
            repo.add_version(
                portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
                Some(Interned::intern("0")),
                None,
                empty_deps(),
            );
            let mut provider = {
                repo.set_use_config(UseConfig::new());
                PortageDependencyProvider::new(repo)
            };
            let a =
                PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
            provider
                .resolve_targets(vec![(a, PortageVersionSet::any())])
                .expect("unsatisfiable REQUIRED_USE of fixed flags must not break the solve")
        };

        let with_ru: std::collections::BTreeSet<String> = build(true)
            .iter()
            .map(|(p, v)| format!("{p}@{v}"))
            .collect();
        let without_ru: std::collections::BTreeSet<String> = build(false)
            .iter()
            .map(|(p, v)| format!("{p}@{v}"))
            .collect();

        assert!(
            with_ru.iter().any(|s| s.contains("app-misc/a")),
            "a must still be selected despite its unsatisfiable REQUIRED_USE"
        );
        assert_eq!(
            with_ru, without_ru,
            "REQUIRED_USE of fixed flags must not change the solution"
        );
    }

    #[test]
    fn ceded_flag_follows_preference() {
        // A SolverDecided flag with no constraint forcing it should take the
        // caller's preferred value: choose_version biases its UseDecision node.
        // Observable via a conditional dep gated on the flag.
        let build = |prefer: bool| {
            let mut repo = InMemoryRepository::new();
            repo.add_version(
                portage_atom::Cpv::parse("dev-libs/b-1.0").unwrap(),
                Some(Interned::intern("0")),
                None,
                empty_deps(),
            );
            repo.add_version_with_iuse(
                portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
                Some(Interned::intern("0")),
                None,
                vec![Interned::intern("flag")],
                PackageDeps {
                    rdepend: DepEntry::parse("flag? ( dev-libs/b )").unwrap(),
                    ..empty_deps()
                },
            );
            let mut cfg = UseConfig::new();
            cfg.solver_decide(Interned::intern("flag"), prefer);
            let mut provider = {
                repo.set_use_config(cfg);
                PortageDependencyProvider::new(repo)
            };
            let a =
                PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
            provider
                .resolve_targets(vec![(a, PortageVersionSet::any())])
                .unwrap()
        };
        let has_b = |sol: &SelectedDependencies<PortagePackage, Version>| {
            sol.iter().any(|(p, _)| p.cpn().package.as_str() == "b")
        };
        assert!(has_b(&build(true)), "prefer=on must enable flag → pull b");
        assert!(
            !has_b(&build(false)),
            "prefer=off must leave flag off → no b"
        );
    }

    // ---- Level-C REQUIRED_USE encoding (Phase 1b) ----

    /// Build `app-misc/a` with the given REQUIRED_USE, ceding x/y/z (preferences
    /// from `prefer`), where each flag pulls a marker dep `dev-libs/p{flag}` when
    /// on. Returns the set of marker package names present in the solution.
    fn solve_required_use(
        ru: crate::required_use::RequiredUse,
        prefer: &[(&str, bool)],
        fixed: &[(&str, bool)],
    ) -> std::collections::BTreeSet<String> {
        let mut repo = InMemoryRepository::new();
        for f in ["w", "x", "y", "z"] {
            repo.add_version(
                portage_atom::Cpv::parse(&format!("dev-libs/p{f}-1.0")).unwrap(),
                Some(Interned::intern("0")),
                None,
                empty_deps(),
            );
        }
        repo.add_version_with_required_use(
            portage_atom::Cpv::parse("app-misc/a-1.0").unwrap(),
            Some(Interned::intern("0")),
            vec![
                Interned::intern("w"),
                Interned::intern("x"),
                Interned::intern("y"),
                Interned::intern("z"),
            ],
            PackageDeps {
                rdepend: DepEntry::parse(
                    "w? ( dev-libs/pw ) x? ( dev-libs/px ) y? ( dev-libs/py ) z? ( dev-libs/pz )",
                )
                .unwrap(),
                ..empty_deps()
            },
            ru,
        );
        let mut cfg = UseConfig::new();
        for (f, p) in prefer {
            cfg.solver_decide(Interned::intern(f), *p);
        }
        for (f, on) in fixed {
            if *on {
                cfg.enable(Interned::intern(f))
            } else {
                cfg.disable(Interned::intern(f))
            }
        }
        let mut provider = {
            repo.set_use_config(cfg);
            PortageDependencyProvider::new(repo)
        };
        let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
        let sol = provider
            .resolve_targets(vec![(a, PortageVersionSet::any())])
            .unwrap();
        sol.iter()
            .filter(|(p, _)| !p.is_virtual() && p.cpn().category.as_str() == "dev-libs")
            .map(|(p, _)| p.cpn().package.as_str().to_string())
            .filter(|n| n.starts_with('p'))
            .collect()
    }

    fn flag(name: &str, negated: bool) -> crate::required_use::RequiredUse {
        crate::required_use::RequiredUse::Flag {
            name: Interned::intern(name),
            negated,
        }
    }

    fn cond(
        name: &str,
        negated: bool,
        entries: Vec<crate::required_use::RequiredUse>,
    ) -> crate::required_use::RequiredUse {
        crate::required_use::RequiredUse::UseConditional {
            flag: Interned::intern(name),
            negated,
            entries,
        }
    }

    #[test]
    fn required_use_exactly_one_picks_one() {
        use crate::required_use::RequiredUse::ExactlyOne;
        // ^^ ( x y ), both ceded off → solver must enable exactly one.
        let got = solve_required_use(
            ExactlyOne(vec![flag("x", false), flag("y", false)]),
            &[("x", false), ("y", false)],
            &[],
        );
        assert_eq!(got.len(), 1, "exactly one marker expected, got {got:?}");
    }

    #[test]
    fn required_use_any_of_enables_at_least_one() {
        use crate::required_use::RequiredUse::AnyOf;
        // || ( x y ), both ceded off → at least one on.
        let got = solve_required_use(
            AnyOf(vec![flag("x", false), flag("y", false)]),
            &[("x", false), ("y", false)],
            &[],
        );
        assert!(!got.is_empty(), "at least one marker expected");
    }

    #[test]
    fn required_use_at_most_one_caps_preferences() {
        use crate::required_use::RequiredUse::AtMostOne;
        // ?? ( x y ), both ceded ON → at most one may stay on.
        let got = solve_required_use(
            AtMostOne(vec![flag("x", false), flag("y", false)]),
            &[("x", true), ("y", true)],
            &[],
        );
        assert!(got.len() <= 1, "at most one marker allowed, got {got:?}");
    }

    #[test]
    fn required_use_conditional_forces_consequent() {
        use crate::required_use::RequiredUse::UseConditional;
        // x? ( y ): x ceded ON (pref) ⇒ y must be on; y prefers OFF but is forced.
        let got = solve_required_use(
            UseConditional {
                flag: Interned::intern("x"),
                negated: false,
                entries: vec![flag("y", false)],
            },
            &[("x", true), ("y", false)],
            &[],
        );
        assert!(got.contains("px"), "x on");
        assert!(got.contains("py"), "y forced on by x? ( y )");
    }

    #[test]
    fn required_use_exactly_one_with_fixed_on_disables_rest() {
        use crate::required_use::RequiredUse::ExactlyOne;
        // ^^ ( x y ): x fixed ON, y ceded (prefers on) → y must be off.
        let got = solve_required_use(
            ExactlyOne(vec![flag("x", false), flag("y", false)]),
            &[("y", true)],
            &[("x", true)],
        );
        assert!(got.contains("px"), "x is the fixed-on choice");
        assert!(
            !got.contains("py"),
            "y must be disabled by ^^ with x fixed on"
        );
    }

    #[test]
    fn required_use_preference_kept_when_unconstrained() {
        use crate::required_use::RequiredUse::AnyOf;
        // || ( x y ) with x preferring ON: the at-least-one is already met by x,
        // y stays at its preferred OFF (no gratuitous flip).
        let got = solve_required_use(
            AnyOf(vec![flag("x", false), flag("y", false)]),
            &[("x", true), ("y", false)],
            &[],
        );
        assert!(got.contains("px"));
        assert!(!got.contains("py"), "y should keep its preferred off");
    }

    #[test]
    fn required_use_exactly_one_keeps_preferred_not_first() {
        use crate::required_use::RequiredUse::ExactlyOne;
        // ^^ ( x y ) with the *second*-listed flag (y) preferred on and already
        // satisfying the group: the solver must keep y, not gratuitously flip to
        // the first-listed x. Guards against choice branches ignoring preference.
        let got = solve_required_use(
            ExactlyOne(vec![flag("x", false), flag("y", false)]),
            &[("x", false), ("y", true)],
            &[],
        );
        assert!(got.contains("py"), "preferred y kept, got {got:?}");
        assert!(
            !got.contains("px"),
            "x not gratuitously enabled, got {got:?}"
        );
    }

    #[test]
    fn required_use_any_of_keeps_preferred_no_extra() {
        use crate::required_use::RequiredUse::AnyOf;
        // || ( x y z ) with only z (last) preferred on: the at-least-one is met,
        // no other flag should be flipped on (the python_targets blowup case).
        let got = solve_required_use(
            AnyOf(vec![flag("x", false), flag("y", false), flag("z", false)]),
            &[("x", false), ("y", false), ("z", true)],
            &[],
        );
        assert!(got.contains("pz"), "preferred z kept");
        assert!(
            !got.contains("px") && !got.contains("py"),
            "no extra flips, got {got:?}"
        );
    }

    #[test]
    fn required_use_nested_exactly_one_under_guard() {
        use crate::required_use::RequiredUse::{ExactlyOne, UseConditional};
        // x? ( ^^ ( y z ) ): x ceded ON, y/z ceded OFF → x stays on and exactly
        // one of y/z is enabled by the nested group.
        let got = solve_required_use(
            UseConditional {
                flag: Interned::intern("x"),
                negated: false,
                entries: vec![ExactlyOne(vec![flag("y", false), flag("z", false)])],
            },
            &[("x", true), ("y", false), ("z", false)],
            &[],
        );
        assert!(got.contains("px"), "x kept on");
        let yz = got.iter().filter(|n| *n == "py" || *n == "pz").count();
        assert_eq!(yz, 1, "exactly one of y/z under the guard, got {got:?}");
    }

    #[test]
    fn required_use_nested_group_inert_when_guard_off() {
        use crate::required_use::RequiredUse::{ExactlyOne, UseConditional};
        // x? ( ^^ ( y z ) ): x ceded OFF (preferred) → the nested ^^ never fires,
        // so y/z keep their preferred off (no gratuitous enable).
        let got = solve_required_use(
            UseConditional {
                flag: Interned::intern("x"),
                negated: false,
                entries: vec![ExactlyOne(vec![flag("y", false), flag("z", false)])],
            },
            &[("x", false), ("y", false), ("z", false)],
            &[],
        );
        assert!(got.is_empty(), "guard off ⇒ nothing forced, got {got:?}");
    }

    #[test]
    fn required_use_nested_conditional_fixed_inner_guard() {
        use crate::required_use::RequiredUse::UseConditional;
        // x? ( y? ( z ) ): x ceded ON, y *fixed* ON (not ceded), z prefers OFF →
        // the inner guard collapses to a constant and z is forced on.
        let got = solve_required_use(
            UseConditional {
                flag: Interned::intern("x"),
                negated: false,
                entries: vec![UseConditional {
                    flag: Interned::intern("y"),
                    negated: false,
                    entries: vec![flag("z", false)],
                }],
            },
            &[("x", true), ("z", false)],
            &[("y", true)],
        );
        assert!(got.contains("px") && got.contains("py"), "x,y on");
        assert!(got.contains("pz"), "z forced on by x? ( y(fixed)? ( z ) )");
    }

    #[test]
    fn required_use_doubly_ceded_chain_forces_consequent() {
        // x? ( y? ( z ) ) with BOTH x and y ceded ON: the clause encoding
        // (¬x ∨ ¬y ∨ z) must fire, and the body-first branch order prefers
        // enabling the consequent over flipping a user-configured guard.
        let got = solve_required_use(
            cond("x", false, vec![cond("y", false, vec![flag("z", false)])]),
            &[("x", true), ("y", true), ("z", false)],
            &[],
        );
        assert!(got.contains("px") && got.contains("py"), "guards kept on");
        assert!(got.contains("pz"), "z forced on by x? ( y? ( z ) )");
    }

    #[test]
    fn required_use_doubly_ceded_chain_inactive_guard_no_flip() {
        // x? ( y? ( z ) ) with y preferring OFF: the clause is already met by
        // the ¬y escape, so nothing is flipped (z stays off).
        let got = solve_required_use(
            cond("x", false, vec![cond("y", false, vec![flag("z", false)])]),
            &[("x", true), ("y", false), ("z", false)],
            &[],
        );
        assert!(got.contains("px"), "x kept on");
        assert!(
            !got.contains("py") && !got.contains("pz"),
            "no flips: {got:?}"
        );
    }

    #[test]
    fn required_use_chain_negated_inner_guard() {
        // x? ( !y? ( z ) ) with x on, y OFF (so the inner guard is active):
        // clause ¬x ∨ y ∨ z; body-first ⇒ z forced on, y stays off.
        let got = solve_required_use(
            cond("x", false, vec![cond("y", true, vec![flag("z", false)])]),
            &[("x", true), ("y", false), ("z", false)],
            &[],
        );
        assert!(got.contains("px"), "x kept on");
        assert!(!got.contains("py"), "y not gratuitously enabled");
        assert!(got.contains("pz"), "z forced on by x? ( !y? ( z ) )");
    }

    #[test]
    fn required_use_triple_ceded_chain() {
        // w? ( x? ( y? ( z ) ) ), all guards ceded ON: depth-3 chain is one
        // 4-literal clause; z is forced on.
        let got = solve_required_use(
            cond(
                "w",
                false,
                vec![cond(
                    "x",
                    false,
                    vec![cond("y", false, vec![flag("z", false)])],
                )],
            ),
            &[("w", true), ("x", true), ("y", true), ("z", false)],
            &[],
        );
        assert!(
            got.contains("pw") && got.contains("px") && got.contains("py"),
            "guards kept on: {got:?}"
        );
        assert!(got.contains("pz"), "z forced on by the depth-3 chain");
    }

    #[test]
    fn required_use_chain_fixed_false_body_escapes_guard() {
        // x? ( y? ( z ) ) with z FIXED off: unsatisfiable body ⇒ one guard
        // must flip off (the escape clause ¬x ∨ ¬y), the other stays on.
        let got = solve_required_use(
            cond("x", false, vec![cond("y", false, vec![flag("z", false)])]),
            &[("x", true), ("y", true)],
            &[("z", false)],
        );
        assert!(!got.contains("pz"), "z is fixed off");
        let guards = got.iter().filter(|n| *n == "px" || *n == "py").count();
        assert_eq!(guards, 1, "exactly one guard escapes, got {got:?}");
    }

    #[test]
    fn required_use_any_of_under_ceded_chain() {
        // x? ( y? ( || ( w z ) ) ), guards ceded ON, w/z OFF: one clause
        // ¬x ∨ ¬y ∨ w ∨ z; at least one of w/z comes on, guards stay on.
        let got = solve_required_use(
            cond(
                "x",
                false,
                vec![cond(
                    "y",
                    false,
                    vec![crate::required_use::RequiredUse::AnyOf(vec![
                        flag("w", false),
                        flag("z", false),
                    ])],
                )],
            ),
            &[("w", false), ("x", true), ("y", true), ("z", false)],
            &[],
        );
        assert!(got.contains("px") && got.contains("py"), "guards kept on");
        let wz = got.iter().filter(|n| *n == "pw" || *n == "pz").count();
        assert!(wz >= 1, "at least one of w/z under the chain, got {got:?}");
    }

    #[test]
    fn required_use_at_most_one_under_ceded_chain() {
        // x? ( y? ( ?? ( w z ) ) ), guards ON, w/z both ON: pairwise clause
        // ¬x ∨ ¬y ∨ ¬w ∨ ¬z; at most one of w/z survives, guards stay on.
        let got = solve_required_use(
            cond(
                "x",
                false,
                vec![cond(
                    "y",
                    false,
                    vec![crate::required_use::RequiredUse::AtMostOne(vec![
                        flag("w", false),
                        flag("z", false),
                    ])],
                )],
            ),
            &[("w", true), ("x", true), ("y", true), ("z", true)],
            &[],
        );
        assert!(got.contains("px") && got.contains("py"), "guards kept on");
        let wz = got.iter().filter(|n| *n == "pw" || *n == "pz").count();
        assert!(wz <= 1, "at most one of w/z under the chain, got {got:?}");
    }

    #[test]
    fn required_use_nested_at_most_one_under_guard() {
        use crate::required_use::RequiredUse::{AtMostOne, UseConditional};
        // x? ( ?? ( y z ) ): x ceded ON, y/z both ceded ON → at most one of y/z
        // may stay on while the guard is active.
        let got = solve_required_use(
            UseConditional {
                flag: Interned::intern("x"),
                negated: false,
                entries: vec![AtMostOne(vec![flag("y", false), flag("z", false)])],
            },
            &[("x", true), ("y", true), ("z", true)],
            &[],
        );
        assert!(got.contains("px"), "x kept on");
        let yz = got.iter().filter(|n| *n == "py" || *n == "pz").count();
        assert!(yz <= 1, "at most one of y/z under the guard, got {got:?}");
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
        let mut provider = PortageDependencyProvider::new(repo);
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
                required_use: None,
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
        let mut provider = PortageDependencyProvider::new(repo);
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
        let mut provider = {
            repo.set_use_config(config);
            PortageDependencyProvider::new(repo)
        };
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

        let provider = PortageDependencyProvider::new(repo);
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
    #[test]
    fn use_dep_from_new_parent_on_installed_target_built_without_flag() {
        // The distlib case: a NEW parent version BDEPENDs `b[flag]`; b is
        // installed at a version whose BUILD lacked `flag`, but the global
        // config has `flag` on (so a naive desired-config check looks
        // satisfied). The requirement must still be raised (rebuild b).
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
            empty_deps(),
        );
        repo.add_version_with_iuse(
            portage_atom::Cpv::parse("app-misc/a-2.0").unwrap(),
            Some(Interned::intern("0")),
            None,
            vec![Interned::intern("flag")],
            PackageDeps {
                bdepend: DepEntry::parse("dev-libs/b[flag(-)?]").unwrap(),
                ..empty_deps()
            },
        );
        let mut cfg = UseConfig::new();
        cfg.enable(Interned::intern("flag"));
        let mut provider = {
            repo.set_use_config(cfg);
            PortageDependencyProvider::new(repo)
        };
        let a = PortagePackage::slotted(Cpn::parse("app-misc/a").unwrap(), Interned::intern("0"));
        let b = PortagePackage::slotted(Cpn::parse("dev-libs/b").unwrap(), Interned::intern("0"));
        provider.add_installed(InstalledPackage {
            package: a.clone(),
            version: Version::parse("1.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![],
            iuse: vec![],
        });
        provider.add_installed(InstalledPackage {
            package: b.clone(),
            version: Version::parse("1.0").unwrap(),
            policy: InstalledPolicy::Favor,
            active_use: vec![], // built WITHOUT `flag`
            iuse: vec![Interned::intern("flag")],
        });
        let sol = provider
            .resolve_targets(vec![(a, PortageVersionSet::any())])
            .unwrap();
        assert!(
            sol.iter()
                .any(|(p, v)| p.cpn().package.as_str() == "a"
                    && v == &Version::parse("2.0").unwrap())
        );
        let req = provider
            .use_flag_requirements()
            .iter()
            .find(|r| r.package.cpn().package.as_str() == "b")
            .cloned();
        let req = req.expect("b[flag] from the new parent must raise a requirement");
        assert!(req.required_enabled.contains(&Interned::intern("flag")));
    }
}
