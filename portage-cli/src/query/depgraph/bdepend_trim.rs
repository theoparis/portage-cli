//! Post-solve trim: drop plan entries only pulled for `BDEPEND` already
//! satisfied on BROOT or by earlier within-run merges.

use std::collections::HashSet;

use portage_atom::{Cpn, Cpv, DepEntry, Version};
use portage_atom_pubgrub::{PortagePackage, UseConfig, UseOverride};

use crate::bdepend_avail::Avail;
use crate::cli::Roots;

use super::effective_use;
use super::repo::{self, RepoData};

/// Context for [`trim_within_run_bdepend`].
pub struct TrimCtx<'a> {
    pub roots: &'a Roots,
    pub data: &'a RepoData,
    pub use_config: &'a UseConfig,
    pub package_use: &'a [(portage_atom::Dep, Vec<UseOverride>)],
    pub root_cpns: &'a HashSet<Cpn>,
    pub reinstall_cpns: &'a HashSet<Cpn>,
}

/// Drop entries that are only needed for `BDEPEND` edges already satisfied by
/// the host/prefix VDB or earlier kept plan entries. No-op when the solver did
/// not include `BDEPEND` (`with_bdeps=false`).
///
/// `full_solution_order` is every real package the solver selected, *before*
/// the caller's "already installed, nothing to display" filter drops entries
/// like `virtual/libcrypt` from `order`. Runtime-requirement scanning must use
/// the full set: an already-installed package invisible in `order` can still
/// be the sole reason some other, not-yet-installed package is required (its
/// own DEPEND/RDEPEND edges don't stop existing just because it needs no
/// action itself). Scanning only `order` made such a dependency look
/// orphaned and wrongly trimmable — see `todo/stage-build-shakeout.md`.
pub fn trim_within_run_bdepend(
    order: Vec<(PortagePackage, Version)>,
    full_solution_order: &[(PortagePackage, Version)],
    with_bdeps: bool,
    ctx: &TrimCtx<'_>,
) -> Vec<(PortagePackage, Version)> {
    if !with_bdeps || order.is_empty() {
        return order;
    }

    let runtime_required = runtime_required_cpns(full_solution_order, ctx);
    let mut kept: Vec<(PortagePackage, Version)> = Vec::with_capacity(order.len());
    let mut kept_indices: Vec<usize> = Vec::with_capacity(order.len());

    for (i, (pkg, ver)) in order.iter().enumerate() {
        let cand = TrimCandidate {
            index: i,
            pkg,
            order: &order,
            kept: &kept,
            kept_indices: &kept_indices,
            ctx,
            runtime_required: &runtime_required,
        };
        if should_keep(&cand) {
            kept.push((pkg.clone(), ver.clone()));
            kept_indices.push(i);
        }
    }

    kept
}

fn runtime_required_cpns(order: &[(PortagePackage, Version)], ctx: &TrimCtx<'_>) -> HashSet<Cpn> {
    let mut out = HashSet::new();
    for (pkg, ver) in order {
        let Some(cache) = repo::find_cache(ctx.data, pkg, ver) else {
            continue;
        };
        let effective =
            effective_use::effective_use(ctx.use_config, ctx.package_use, pkg, ver, cache);
        for field in [
            &cache.metadata.depend,
            &cache.metadata.rdepend,
            &cache.metadata.pdepend,
            &cache.metadata.idepend,
        ] {
            collect_cpns_from_entries(&DepEntry::evaluate_use(field, &effective), &mut out);
        }
    }
    out
}

fn collect_cpns_from_entries(entries: &[DepEntry], out: &mut HashSet<Cpn>) {
    for e in entries {
        match e {
            DepEntry::Atom(dep) if dep.blocker.is_none() => {
                out.insert(dep.cpn);
            }
            DepEntry::AllOf(c) | DepEntry::AnyOf(c) => collect_cpns_from_entries(c, out),
            DepEntry::ExactlyOneOf(c) | DepEntry::AtMostOneOf(c) => {
                collect_cpns_from_entries(c, out);
            }
            _ => {}
        }
    }
}

struct TrimCandidate<'a, 'b> {
    index: usize,
    pkg: &'a PortagePackage,
    order: &'a [(PortagePackage, Version)],
    kept: &'a [(PortagePackage, Version)],
    kept_indices: &'a [usize],
    ctx: &'a TrimCtx<'b>,
    runtime_required: &'a HashSet<Cpn>,
}

fn should_keep(cand: &TrimCandidate<'_, '_>) -> bool {
    let cpn = *cand.pkg.cpn();
    if cand.ctx.root_cpns.contains(&cpn) || cand.ctx.reinstall_cpns.contains(&cpn) {
        return true;
    }
    if cand.runtime_required.contains(&cpn) {
        return true;
    }

    for (j, (consumer, consumer_ver)) in cand.order.iter().enumerate().skip(cand.index + 1) {
        let avail = avail_for_consumer(j, cand.kept, cand.kept_indices, cand.ctx.roots);
        let Some(cache) = repo::find_cache(cand.ctx.data, consumer, consumer_ver) else {
            continue;
        };
        let effective = effective_use::effective_use(
            cand.ctx.use_config,
            cand.ctx.package_use,
            consumer,
            consumer_ver,
            cache,
        );
        let bdepend = DepEntry::evaluate_use(&cache.metadata.bdepend, &effective);
        if avail.has_unsatisfied_atom_for_cpn(&bdepend, cpn) {
            return true;
        }
    }

    false
}

fn avail_for_consumer(
    consumer_index: usize,
    kept: &[(PortagePackage, Version)],
    kept_indices: &[usize],
    roots: &Roots,
) -> Avail {
    let mut avail = Avail::initial_bdepend(roots);
    for (k, (pkg, ver)) in kept.iter().enumerate() {
        if kept_indices[k] < consumer_index {
            let cpv = Cpv::new(*pkg.cpn(), ver.clone());
            avail.record_merge(cpv, pkg.merge_root());
        }
    }
    avail
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use portage_atom_pubgrub::UseConfig;
    use portage_metadata::CacheEntry;

    use super::*;
    use crate::cli::Roots;

    fn empty_roots() -> Roots {
        Roots::default()
    }

    /// Build a `RepoData` from `(cpv, md5-cache-text)` pairs, one version per CPN.
    fn repo_from(entries: &[(&str, &str)]) -> RepoData {
        let mut versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
        let mut cpns = Vec::new();
        for (cpv_str, text) in entries {
            let cpv = Cpv::parse(cpv_str).unwrap();
            let entry = CacheEntry::parse(text).unwrap();
            cpns.push(cpv.cpn);
            versions.entry(cpv.cpn).or_default().push((cpv, entry));
        }
        RepoData {
            cpns,
            versions,
            repo_name: "test".into(),
            repo_of: HashMap::new(),
        }
    }

    /// Regression test for the bug found chasing `sys-apps/shadow` missing
    /// `sys-libs/libxcrypt`: an already-installed package (here
    /// `virtual/lib`, standing in for `virtual/libcrypt`) is correctly
    /// excluded from the *displayed* `order` — but it's still the sole
    /// reason `sys-libs/reallib` (standing in for `sys-libs/libxcrypt`) is
    /// required. `runtime_required_cpns` must see `virtual/lib`'s RDEPEND
    /// edge via `full_solution_order` even though `virtual/lib` itself never
    /// appears in `order`, or `reallib` gets wrongly trimmed as orphaned.
    #[test]
    fn already_installed_package_excluded_from_order_still_pins_its_rdepend() {
        let data = repo_from(&[
            (
                "sys-apps/consumer-1",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nRDEPEND=virtual/lib\n",
            ),
            (
                "virtual/lib-1",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nRDEPEND=sys-libs/reallib\n",
            ),
            (
                "sys-libs/reallib-1",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\n",
            ),
        ]);

        let consumer = (
            PortagePackage::unslotted(Cpn::parse("sys-apps/consumer").unwrap()),
            Version::parse("1").unwrap(),
        );
        let virtual_lib = (
            PortagePackage::unslotted(Cpn::parse("virtual/lib").unwrap()),
            Version::parse("1").unwrap(),
        );
        let reallib = (
            PortagePackage::unslotted(Cpn::parse("sys-libs/reallib").unwrap()),
            Version::parse("1").unwrap(),
        );

        // `order`: what's actually displayed/merged — `virtual/lib` is
        // already installed and excluded, matching the real bug scenario.
        let order = vec![consumer.clone(), reallib.clone()];
        let full_solution_order = vec![consumer.clone(), virtual_lib, reallib.clone()];

        let use_config = UseConfig::new();
        let root_cpns: HashSet<Cpn> = [*consumer.0.cpn()].into_iter().collect();
        let reinstall = HashSet::new();
        let roots = empty_roots();
        let ctx = TrimCtx {
            roots: &roots,
            data: &data,
            use_config: &use_config,
            package_use: &[],
            root_cpns: &root_cpns,
            reinstall_cpns: &reinstall,
        };

        let kept = trim_within_run_bdepend(order.clone(), &full_solution_order, true, &ctx);
        assert!(
            kept.iter().any(|(p, _)| p.cpn() == reallib.0.cpn()),
            "reallib must survive: it's required via virtual/lib's RDEPEND, \
             even though virtual/lib itself isn't in the displayed order"
        );

        // Negative control: with the pre-fix behaviour (scanning only `order`,
        // which excludes `virtual/lib`), `reallib` looks orphaned and is
        // wrongly dropped — demonstrating the bug this fix closes.
        let buggy = trim_within_run_bdepend(order.clone(), &order, true, &ctx);
        assert!(
            !buggy.iter().any(|(p, _)| p.cpn() == reallib.0.cpn()),
            "sanity check: scanning only `order` reproduces the original bug"
        );
    }

    #[test]
    fn no_op_when_with_bdeps_off() {
        let pkg = PortagePackage::unslotted(Cpn::parse("app-misc/a").unwrap());
        let ver = Version::parse("1.0").unwrap();
        let order = vec![(pkg, ver)];
        let data = RepoData {
            cpns: Vec::new(),
            versions: HashMap::new(),
            repo_name: "gentoo".into(),
            repo_of: HashMap::new(),
        };
        let use_config = UseConfig::new();
        let root_cpns = HashSet::new();
        let reinstall = HashSet::new();
        let roots = empty_roots();
        let ctx = TrimCtx {
            roots: &roots,
            data: &data,
            use_config: &use_config,
            package_use: &[],
            root_cpns: &root_cpns,
            reinstall_cpns: &reinstall,
        };
        let out = trim_within_run_bdepend(order.clone(), &order, false, &ctx);
        assert_eq!(out.len(), order.len());
    }
}
