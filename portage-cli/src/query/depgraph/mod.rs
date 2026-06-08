mod autounmask;
mod conflicts;
mod download_size;
mod installed;
mod output;
mod package_use;
mod repo;
mod required_use;
mod use_env;

use std::collections::HashMap;

use camino::Utf8Path;
use gentoo_core::Arch;
use portage_atom::interner::Interned;
use portage_atom::{Cpn, Cpv, Dep, Operator, Version};
use portage_atom_pubgrub::{
    InstalledPackage as SolverInstalledPackage, InstalledPolicy, PortageDependencyProvider,
    PortagePackage, PortageVersionSet, UseFlagRequirement,
};
use portage_repo::Repository;

use crate::cli::DepgraphFormat;

pub struct DepgraphOpts<'a> {
    pub repo_path: &'a Utf8Path,
    pub atoms: &'a [String],
    pub arch: &'a Arch,
    pub format: DepgraphFormat,
    pub verbose: bool,
    pub empty: bool,
    pub autounmask: bool,
    pub autounmask_write: bool,
    pub autosolve_use: bool,
    pub root: Option<&'a Utf8Path>,
}

pub async fn depgraph(opts: DepgraphOpts<'_>) -> anyhow::Result<()> {
    let DepgraphOpts { repo_path, atoms, arch, format, verbose, empty, autounmask, autounmask_write, autosolve_use, root } = opts;
    let repo = Repository::open(repo_path)
        .map_err(|e| anyhow::anyhow!("failed to open repo at {repo_path}: {e}"))?;

    let (data, installed_entries, use_env_result) = tokio::join!(
        repo::load_repo(&repo),
        async { installed::load_installed() },
        use_env::build_use_env(&repo, root),
    );
    let use_env = use_env_result?;
    let use_env::UseEnv {
        config: use_config,
        expand: use_expand,
        expand_hidden: use_expand_hidden,
        package_use,
        package_mask,
        accept_keywords,
        accept_license,
        distdir,
    } = use_env;

    let installed_cpvs: std::collections::HashSet<Cpv> = installed_entries
        .iter()
        .map(|e| Cpv::new(e.cpn, e.version.clone()))
        .collect();

    let mut installed: HashMap<Cpn, HashMap<String, Version>> = HashMap::new();
    for e in &installed_entries {
        let slot_key = e.slot.clone().unwrap_or_default();
        installed
            .entry(e.cpn)
            .or_default()
            .insert(slot_key, e.version.clone());
    }

    let mut root_deps = Vec::new();
    let mut root_cpns: std::collections::HashSet<Cpn> = std::collections::HashSet::new();
    for target in atoms {
        let dep = Dep::parse(target)
            .map_err(|e| anyhow::anyhow!("bad atom '{target}': {e}"))?;
        root_cpns.insert(dep.cpn);
        let pkg = repo::target_package(
            &data, &dep, arch, &accept_keywords, &package_mask, &accept_license,
        );
        let vs = match &dep.version {
            Some(v) => {
                let op = dep.op.unwrap_or(Operator::GreaterOrEqual);
                PortageVersionSet::from_operator(op, dep.glob, v.clone())
            }
            None => PortageVersionSet::any(),
        };
        root_deps.push((pkg, vs));
    }

    // Build a provider (with the given cede policy) and run the solve. Factored
    // so a failed --autosolve-use attempt can fall back to a fixed-USE (Level A)
    // solve instead of erroring — matching the doc invariant.
    let build_and_solve = |autosolve_use: bool| {
        let adapter = repo::Adapter {
            data: &data,
            arch,
            accept_keywords: &accept_keywords,
            package_mask: &package_mask,
            accept_license: &accept_license,
            use_config: &use_config,
            package_use: &package_use,
            autosolve_use,
        };
        let mut provider = PortageDependencyProvider::new(adapter);
        if !empty {
            for e in &installed_entries {
                let pkg = match e.slot.as_deref().filter(|s| !s.is_empty()) {
                    Some(s) => PortagePackage::slotted(e.cpn, Interned::intern(s)),
                    None => PortagePackage::unslotted(e.cpn),
                };
                provider.add_installed(SolverInstalledPackage {
                    package: pkg,
                    version: e.version.clone(),
                    policy: InstalledPolicy::Favor,
                    active_use: e.active_use.clone(),
                    iuse: e.iuse.clone(),
                });
            }
        }
        let result = provider.resolve_targets(root_deps.clone());
        (provider, result)
    };

    let (provider, solution) = {
        let (provider, result) = build_and_solve(autosolve_use);
        match result {
            Ok(sol) => (provider, sol),
            Err(_) if autosolve_use => {
                // REQUIRED_USE could not be auto-satisfied; fall back to a
                // fixed-USE solve so the plan + Level-A advisory still appear.
                eprintln!(
                    "!!! --autosolve-use could not satisfy REQUIRED_USE; \
                     falling back to a fixed-USE plan."
                );
                let (provider, result) = build_and_solve(false);
                let sol = result
                    .map_err(|e2| anyhow::anyhow!("resolution failed: {:?}", e2))?;
                (provider, sol)
            }
            Err(e) => return Err(anyhow::anyhow!("resolution failed: {:?}", e)),
        }
    };

    // Level-C: fold the solver's chosen ceded-flag values back into the
    // effective USE used for display, the REQUIRED_USE check, and autounmask, by
    // appending synthetic `=cpv flag` package.use entries. With --autosolve-use
    // off this is empty and `package_use` is unchanged (parity preserved).
    let ceded = provider.solved_use_decisions();
    let package_use: Vec<(Dep, Vec<String>)> = if ceded.is_empty() {
        package_use
    } else {
        let mut by_cpn: HashMap<Cpn, Vec<&portage_atom_pubgrub::CededFlag>> = HashMap::new();
        for c in &ceded {
            by_cpn.entry(c.cpn).or_default().push(c);
        }
        let mut combined = package_use.clone();
        for (pkg, ver) in solution.iter() {
            if pkg.is_virtual() {
                continue;
            }
            if let Some(flags) = by_cpn.get(pkg.cpn()) {
                let atom = format!("={}/{}-{}", pkg.cpn().category, pkg.cpn().package, ver);
                if let Ok(dep) = Dep::parse(&atom) {
                    let tokens = flags
                        .iter()
                        .map(|c| {
                            if c.value {
                                c.flag.as_str().to_string()
                            } else {
                                format!("-{}", c.flag.as_str())
                            }
                        })
                        .collect();
                    combined.push((dep, tokens));
                }
            }
        }
        combined
    };

    if verbose {
        output::report_dropped_deps(provider.dropped_deps(), &data, arch.as_str());
    }

    // Autounmask: detect filtered candidates from dropped deps.
    let autounmask_candidates = repo::find_autounmask_candidates(
        &data,
        provider.dropped_deps(),
        arch.as_str(),
        &accept_keywords,
        &package_mask,
        &accept_license,
    );

    let root_pkgs: Vec<PortagePackage> = root_deps.iter().map(|(p, _)| p.clone()).collect();

    // A candidate is only actionable if:
    // 1. Its CPN is not already in the solution (an available version satisfies the dep).
    // 2. Its CPN is referenced in the raw dep data of at least one package in the
    //    NEW install plan — deps of already-installed packages were already satisfied
    //    when those packages were built and don't need fixing now.
    let solution_cpns: std::collections::HashSet<Cpn> = solution
        .iter()
        .filter(|(p, _)| !p.is_virtual())
        .map(|(p, _)| *p.cpn())
        .collect();

    // Packages that need a same-version rebuild (USE change) must stay in the
    // merge list even though their installed CPV is unchanged — keep them in
    // their topological position rather than appending them after the target.
    let reinstall_cpns: std::collections::HashSet<Cpn> = provider
        .reinstall_deps()
        .iter()
        .map(|r| *r.package.cpn())
        .collect();

    // When a rebuild is forced on an installed package and a newer version is
    // available, favour the upgrade: build the newest version rather than
    // rebuilding the installed one (matching emerge, and required when the
    // installed version has been removed from the tree — it can't be rebuilt).
    let upgrades: HashMap<Cpn, Version> = provider
        .reinstall_deps()
        .iter()
        .filter_map(|r| r.upgrade_to.as_ref().map(|v| (*r.package.cpn(), v.clone())))
        .collect();

    let mut order: Vec<_> = provider
        .install_order(&solution)
        .into_iter()
        .filter(|(pkg, ver)| {
            if pkg.is_virtual() {
                return false;
            }
            let cpv = Cpv::new(*pkg.cpn(), ver.clone());
            // Drop packages already installed at this version, except:
            //  - same-version USE rebuilds (reinstall_cpns), and
            //  - explicitly-requested targets, which emerge reinstalls by
            //    default ([ebuild R]) even when already at the best version.
            !installed_cpvs.contains(&cpv)
                || reinstall_cpns.contains(pkg.cpn())
                || root_cpns.contains(pkg.cpn())
        })
        .map(|(pkg, ver)| {
            // Apply the favoured upgrade version if one was recorded.
            let ver = upgrades.get(pkg.cpn()).cloned().unwrap_or(ver);
            (pkg, ver)
        })
        .collect();

    // Fallback: any reinstall the solver didn't route through install_order
    // (rare) is appended so it is not silently dropped.
    {
        let in_order: std::collections::HashSet<Cpn> =
            order.iter().map(|(pkg, _)| *pkg.cpn()).collect();
        let to_reinstall: Vec<(PortagePackage, Version)> = provider
            .reinstall_deps()
            .into_iter()
            .filter(|r| !in_order.contains(r.package.cpn()))
            .map(|r| {
                let ver = r.upgrade_to.as_ref().unwrap_or(&r.version).clone();
                (r.package.clone(), ver)
            })
            .collect();
        order.extend(to_reinstall);
    }

    let edges: Vec<_> = provider
        .dependency_graph(&solution)
        .into_iter()
        .filter(|e| !e.from.0.is_virtual() && !e.to.0.is_virtual())
        .collect();

    // Emerge convention: list the explicitly-requested target(s) last.  Only
    // move a target that nothing else depends on (not a `to` in any edge), so
    // the order stays topologically valid for `em -p A B` where one target is a
    // dependency of another.
    {
        let depended_upon: std::collections::HashSet<Cpn> =
            edges.iter().map(|e| *e.to.0.cpn()).collect();
        let (targets, rest): (Vec<_>, Vec<_>) = order.into_iter().partition(|(pkg, _)| {
            root_cpns.contains(pkg.cpn()) && !depended_upon.contains(pkg.cpn())
        });
        order = rest;
        order.extend(targets);
    }

    let flag_reqs: HashMap<&PortagePackage, &UseFlagRequirement> = provider
        .use_flag_requirements()
        .iter()
        .map(|r| (&r.package, r))
        .collect();

    let portage_dir = root
        .unwrap_or(camino::Utf8Path::new("/"))
        .join("etc/portage");

    // CPNs referenced in the raw dep data of newly-installed packages.
    let new_needed_cpns: std::collections::HashSet<Cpn> = order
        .iter()
        .filter(|(pkg, _)| !pkg.is_virtual())
        .flat_map(|(pkg, ver)| repo::cpns_for(&data, pkg.cpn(), ver))
        .collect();

    let autounmask_candidates: Vec<_> = autounmask_candidates
        .into_iter()
        .filter(|c| {
            !solution_cpns.contains(&c.cpv.cpn)
                && new_needed_cpns.contains(&c.cpv.cpn)
        })
        .collect();

    // Report in order of severity: mask → keywords → USE → license.
    // --autounmask: show; --autounmask-write: show + write.
    if (autounmask || autounmask_write) && !autounmask_candidates.is_empty() {
        autounmask::report(&autounmask_candidates);
        if autounmask_write {
            autounmask::write(&autounmask_candidates, &portage_dir)?;
        }
    }

    {
        let all_reqs: Vec<_> = provider.use_flag_requirements().to_vec();
        let pkg_use_entries = package_use::build_entries(&all_reqs, atoms, &edges, &use_config, &package_use);
        if (autounmask || autounmask_write) && !pkg_use_entries.is_empty() {
            package_use::report(&pkg_use_entries);
            if autounmask_write {
                package_use::write(&pkg_use_entries, &portage_dir.join("package.use"))?;
            }
        }
    }

    match format {
        DepgraphFormat::Pretty => {
            // Verbose mode shows per-package download size and a total; skip the
            // Manifest/DISTDIR work entirely in plain mode.
            let sizes = if verbose {
                download_size::compute(repo_path, &distdir, &data, &order, &use_config, &package_use)
            } else {
                HashMap::new()
            };
            output::print_pretty(&data, &order, &installed, &use_config, &package_use, &use_expand, &use_expand_hidden, &flag_reqs, &sizes, verbose)
        }
        DepgraphFormat::Json => {
            output::print_json(&data, &order, &edges, &installed, &flag_reqs)
        }
        DepgraphFormat::Tree => {
            let roots: Vec<_> = root_pkgs
                .iter()
                .filter_map(|pkg| {
                    let ver = edges
                        .iter()
                        .find_map(|e| {
                            if &e.from.0 == pkg { Some(e.from.1.clone()) }
                            else if &e.to.0 == pkg { Some(e.to.1.clone()) }
                            else { None }
                        })
                        .or_else(|| order.iter().find(|(p, _)| p == pkg).map(|(_, v)| v.clone()));
                    ver.map(|v| (pkg.clone(), v))
                })
                .collect();
            output::print_tree(&roots, &edges, &installed_cpvs)
        }
    }

    // Advisory warnings are emitted after the plan so the merge list reads
    // first and the caveats follow it (emerge lists issues at the bottom too).
    // These are non-fatal: the plan is still produced.
    //
    //  - reverse-dependency constraints: a complete-graph check that emerge's
    //    default targeted `-p` skips (e.g. upgrading docutils past an installed
    //    package's `<` bound);
    //  - blockers (`!foo` / `!!foo`) and `::repo` constraints, which the solver
    //    does not model;
    //  - REQUIRED_USE, evaluated per-package against its effective USE.
    {
        let proposed: HashMap<Cpn, Version> = order
            .iter()
            .filter(|(pkg, _)| !pkg.is_virtual())
            .map(|(pkg, ver)| (*pkg.cpn(), ver.clone()))
            .collect();
        let dep_conflicts = conflicts::find_conflicts(&installed_entries, &proposed);
        if !dep_conflicts.is_empty() {
            output::report_conflicts(&dep_conflicts);
        }

        let mut violations = provider.check_blockers(&solution);
        violations.extend(provider.check_repo_constraints(&solution));
        if !violations.is_empty() {
            output::report_solver_violations(&violations);
        }

        let ru_violations =
            required_use::find_violations(&data, &order, &use_config, &package_use);
        if !ru_violations.is_empty() {
            output::report_required_use(&ru_violations);
        }

        // Level-C: report the flags the solver flipped from their configured
        // value to satisfy REQUIRED_USE (they appear set in the plan via the
        // synthetic package.use above; this tells the user what changed).
        let flips: Vec<&portage_atom_pubgrub::CededFlag> =
            ceded.iter().filter(|c| c.flipped).collect();
        if !flips.is_empty() {
            output::report_autosolved_use(&flips);
        }
    }

    Ok(())
}
