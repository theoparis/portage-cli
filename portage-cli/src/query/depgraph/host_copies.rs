//! Native-offset host build-copies (Tier 1 `--root` for a Gentoo host).
//!
//! When building into an offset ROOT with the host (`/`) as `BROOT`/`ESYSROOT`,
//! a target package's build-time edges (`DEPEND`, `BDEPEND`, `IDEPEND`) that
//! the host *lacks* must be built on the host first so the target can compile
//! and link against them. emerge lists these as a second merge `to /` alongside
//! the ROOT runtime copy. Example: `net-misc/curl`'s `DEPEND="${RDEPEND}"`
//! includes `net-libs/{nghttp2,nghttp3,ngtcp2}`; if the host lacks them, emerge
//! merges each twice — once `to /eroot/` (runtime) and once `to /` (build).
//!
//! This is computed as a **post-solve closure walk** over the finalized Target
//! plan, NOT inside the solver: the dual `(package, merge_root)` solver nodes
//! share `PackageData` via `host_aliases`, and introducing `pkg@Host` into a
//! single solve balloons the Target closure (the Tier 1 aliasing blocker, see
//! `todo/nonemptytree-bdeps-gap.md`). Keeping the Target solve single-rooted
//! preserves its parity (12 packages for curl); the host copies are derived
//! against the host VDB afterwards.
//!
//! Host copies reuse the Target solve's version for a CPN (same arch, same
//! repo), falling back to the newest accepted repo version for build-only deps
//! absent from the Target plan. The walk is bounded by the host VDB, which
//! already provides the toolchain (gcc, python, …) — so only the genuinely
//! missing libraries are scheduled.

use std::collections::{HashMap, HashSet, VecDeque};

use portage_atom::{Cpn, Cpv, Dep, DepEntry, Version};
use portage_atom_pubgrub::{MergeRoot, PortagePackage};

use crate::bdepend_avail::{Avail, unsatisfied_cpns, vdb_cpvs};

use super::effective_use;
use super::repo::{self, RepoData};
use super::root_aware::CrossContext;

/// Static inputs shared across the walk.
struct Ctx<'a> {
    data: &'a RepoData,
    use_config: &'a portage_atom_pubgrub::UseConfig,
    package_use: &'a [(Dep, Vec<String>)],
    target_ver: &'a HashMap<Cpn, (Version, PortagePackage)>,
}

/// Mutable walk state: host availability (VDB + emitted copies), seen-set,
/// and the work queue.
struct Walk {
    avail: Avail,
    seen: HashSet<Cpn>,
    queue: VecDeque<Cpn>,
}

/// Compute the host (`MergeRoot::Host`) build-copies for a native offset
/// (`--root`/`--prefix`, same arch) from the finalized Target `order`.
///
/// Returns `[]` for non-native-offset builds (plain native, cross-arch, host).
pub fn compute(
    target_order: &[(PortagePackage, Version)],
    data: &RepoData,
    use_config: &portage_atom_pubgrub::UseConfig,
    package_use: &[(Dep, Vec<String>)],
    cross: &CrossContext,
) -> Vec<(PortagePackage, Version)> {
    // Only a native offset (same-arch, target != host) schedules host
    // build-copies; cross-arch uses the solver's dual-root path, plain native
    // has BROOT == ROOT.
    if !cross.active || cross.is_cross_arch() {
        return Vec::new();
    }

    // CPN -> (version, target package) for version reuse: a host copy of a
    // package also built for Target uses the Target version (same arch/repo).
    let target_ver: HashMap<Cpn, (Version, PortagePackage)> = target_order
        .iter()
        .filter(|(p, _)| p.merge_root() == MergeRoot::Target)
        .map(|(p, v)| (*p.cpn(), (v.clone(), p.clone())))
        .collect();
    let ctx = Ctx {
        data,
        use_config,
        package_use,
        target_ver: &target_ver,
    };

    // Host availability starts as the host VDB (BROOT == `/`) and grows with
    // each host copy, mirroring `preflight`'s within-run visibility. Read the
    // VDB once; the seen-set is derived from the same entries.
    let host_installed = vdb_cpvs(None);
    let mut walk = Walk {
        seen: host_installed.iter().map(|(cpv, _)| cpv.cpn).collect(),
        avail: Avail::from_cpvs(host_installed),
        queue: VecDeque::new(),
    };
    let mut copies: Vec<(PortagePackage, Version)> = Vec::new();

    // Seed: every built Target package's build edges unsatisfied on the host.
    for (pkg, ver) in target_order
        .iter()
        .filter(|(p, _)| p.merge_root() == MergeRoot::Target)
    {
        enqueue_unsatisfied(&ctx, &mut walk, pkg, ver);
    }

    // Process: resolve a version, emit the host copy, grow host availability,
    // and recurse into the copy's own build edges (bounded by the host VDB).
    while let Some(cpn) = walk.queue.pop_front() {
        let Some((ver, pkg)) = resolve(cpn, &ctx) else {
            continue;
        };
        let host_pkg = pkg.at_merge_root(MergeRoot::Host);
        let cpv = Cpv::new(cpn, ver.clone());
        walk.avail.record_merge_bdepend(cpv);
        copies.push((host_pkg.clone(), ver.clone()));
        enqueue_unsatisfied(&ctx, &mut walk, &host_pkg, &ver);
    }

    copies
}

/// Append the CPNs of `pkg`'s unsatisfied-on-host `DEPEND`/`BDEPEND`/`IDEPEND`
/// edges to `walk.queue` (and `walk.seen`), folding USE conditionals with the
/// package's effective flags.
fn enqueue_unsatisfied(ctx: &Ctx<'_>, walk: &mut Walk, pkg: &PortagePackage, ver: &Version) {
    let Some(cache) = repo::find_cache(ctx.data, pkg, ver) else {
        return;
    };
    let effective = effective_use::effective_use(ctx.use_config, ctx.package_use, pkg, ver, cache);
    for class in [
        &cache.metadata.depend,
        &cache.metadata.bdepend,
        &cache.metadata.idepend,
    ] {
        let entries = DepEntry::evaluate_use(class, &effective);
        for cpn in unsatisfied_cpns(&entries, &walk.avail) {
            if walk.seen.insert(cpn) {
                walk.queue.push_back(cpn);
            }
        }
    }
}

/// Resolve `(version, package)` for a host copy of `cpn`: the Target plan's
/// version when the CPN is also built for Target, else the newest accepted repo
/// version. `None` when the CPN is absent from the repo.
fn resolve(cpn: Cpn, ctx: &Ctx<'_>) -> Option<(Version, PortagePackage)> {
    if let Some((v, p)) = ctx.target_ver.get(&cpn) {
        return Some((v.clone(), p.clone()));
    }
    let versions = ctx.data.versions.get(&cpn)?;
    let (cpv, cache) = versions
        .iter()
        .max_by(|a, b| a.0.version.cmp(&b.0.version))?;
    let slot = &cache.metadata.slot.slot;
    let pkg = if slot.as_str() == "0" {
        PortagePackage::unslotted(cpn)
    } else {
        PortagePackage::slotted(cpn, *slot)
    };
    Some((cpv.version.clone(), pkg))
}
