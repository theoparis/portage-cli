mod installed;
mod output;
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

pub async fn depgraph(
    repo_path: &Utf8Path,
    atoms: &[String],
    arch: &Arch,
    format: DepgraphFormat,
) -> anyhow::Result<()> {
    let repo = Repository::open(repo_path)
        .map_err(|e| anyhow::anyhow!("failed to open repo at {repo_path}: {e}"))?;

    let (data, installed_entries, (use_config, use_expand, package_use)) = tokio::join!(
        repo::load_repo(&repo),
        async { installed::load_installed() },
        use_env::build_use_config(&repo),
    );

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

    let adapter = repo::Adapter { data: &data, arch };
    let mut provider = PortageDependencyProvider::new(adapter, use_config.clone(), &package_use);

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

    let mut root_deps = Vec::new();
    for target in atoms {
        let dep = Dep::parse(target)
            .map_err(|e| anyhow::anyhow!("bad atom '{target}': {e}"))?;
        let pkg = repo::target_package(&data, &dep, arch);
        let vs = match &dep.version {
            Some(v) => {
                let op = dep.op.unwrap_or(Operator::GreaterOrEqual);
                PortageVersionSet::from_operator(op, dep.glob, v.clone())
            }
            None => PortageVersionSet::any(),
        };
        root_deps.push((pkg, vs));
    }

    output::report_dropped_deps(provider.dropped_deps(), &data, arch.as_str());

    let solution = provider
        .resolve_targets(root_deps)
        .map_err(|e| anyhow::anyhow!("resolution failed: {:?}", e))?;

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

    match format {
        DepgraphFormat::Pretty => {
            output::print_pretty(&data, &order, &installed, &use_config, &use_expand, &flag_reqs)
        }
        DepgraphFormat::Json => {
            output::print_json(&data, &order, &edges, &installed, &flag_reqs)
        }
    }

    Ok(())
}
