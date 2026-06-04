mod autounmask;
mod conflicts;
mod installed;
mod output;
mod package_use;
mod repo;
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
    pub root: Option<&'a Utf8Path>,
}

pub async fn depgraph(opts: DepgraphOpts<'_>) -> anyhow::Result<()> {
    let DepgraphOpts { repo_path, atoms, arch, format, verbose, empty, autounmask, autounmask_write, root } = opts;
    let repo = Repository::open(repo_path)
        .map_err(|e| anyhow::anyhow!("failed to open repo at {repo_path}: {e}"))?;

    let (data, installed_entries, use_env_result) = tokio::join!(
        repo::load_repo(&repo),
        async { installed::load_installed() },
        use_env::build_use_env(&repo),
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

    let adapter = repo::Adapter {
        data: &data,
        arch,
        accept_keywords: &accept_keywords,
        package_mask: &package_mask,
        accept_license: &accept_license,
    };
    let mut provider = PortageDependencyProvider::new(adapter, use_config.clone(), &package_use);

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

    let mut root_deps = Vec::new();
    for target in atoms {
        let dep = Dep::parse(target)
            .map_err(|e| anyhow::anyhow!("bad atom '{target}': {e}"))?;
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

    if verbose {
        output::report_dropped_deps(provider.dropped_deps(), &data, arch.as_str());
    }

    // Autounmask: detect filtered candidates from dropped deps before solving.
    let autounmask_candidates = repo::find_autounmask_candidates(
        &data,
        provider.dropped_deps(),
        arch.as_str(),
        &accept_keywords,
        &package_mask,
        &accept_license,
    );

    let root_pkgs: Vec<PortagePackage> = root_deps.iter().map(|(p, _)| p.clone()).collect();
    let solution = provider
        .resolve_targets(root_deps)
        .map_err(|e| anyhow::anyhow!("resolution failed: {:?}", e))?;

    // Drop autounmask candidates whose CPN is already satisfied by the solution
    // (e.g. an older installed version of a virtual satisfies the dep — newer
    // masked versions were dropped but aren't blocking anything).
    let solution_cpns: std::collections::HashSet<Cpn> = solution
        .iter()
        .filter(|(p, _)| !p.is_virtual())
        .map(|(p, _)| *p.cpn())
        .collect();
    let autounmask_candidates: Vec<_> = autounmask_candidates
        .into_iter()
        .filter(|c| !solution_cpns.contains(&c.cpv.cpn))
        .collect();

    let mut order: Vec<_> = provider
        .install_order(&solution)
        .into_iter()
        .filter(|(pkg, ver)| {
            if pkg.is_virtual() {
                return false;
            }
            let cpv = Cpv::new(*pkg.cpn(), ver.clone());
            !installed_cpvs.contains(&cpv)
        })
        .collect();

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

    // Post-solve: check installed packages' constraints against proposed changes.
    {
        let proposed: HashMap<Cpn, Version> = order
            .iter()
            .filter(|(pkg, _)| !pkg.is_virtual())
            .map(|(pkg, ver)| (*pkg.cpn(), ver.clone()))
            .collect();
        let slot_conflicts = conflicts::find_conflicts(&installed_entries, &proposed);
        if !slot_conflicts.is_empty() {
            output::report_conflicts(&slot_conflicts);
        }
    }

    let edges: Vec<_> = provider
        .dependency_graph(&solution)
        .into_iter()
        .filter(|e| !e.from.0.is_virtual() && !e.to.0.is_virtual())
        .collect();

    let flag_reqs: HashMap<&PortagePackage, &UseFlagRequirement> = provider
        .use_flag_requirements()
        .iter()
        .map(|r| (&r.package, r))
        .collect();

    let portage_dir = root
        .unwrap_or(camino::Utf8Path::new("/"))
        .join("etc/portage");

    // Report in order of severity: mask → keywords → USE → license.
    // Dropped-dep autounmask is only shown when explicitly requested —
    // a successful solve means nothing is actually blocked.
    if (autounmask || autounmask_write) && !autounmask_candidates.is_empty() {
        autounmask::report(&autounmask_candidates);
        if autounmask_write {
            autounmask::write(&autounmask_candidates, &portage_dir)?;
        }
    }

    {
        let all_reqs: Vec<_> = provider.use_flag_requirements().to_vec();
        let pkg_use_entries = package_use::build_entries(&all_reqs, atoms, &edges);
        if !pkg_use_entries.is_empty() {
            package_use::report(&pkg_use_entries);
            if autounmask_write {
                package_use::write(&pkg_use_entries, &portage_dir.join("package.use"))?;
            }
        }
    }

    match format {
        DepgraphFormat::Pretty => {
            output::print_pretty(&data, &order, &installed, &use_config, &package_use, &use_expand, &use_expand_hidden, &flag_reqs)
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

    Ok(())
}
