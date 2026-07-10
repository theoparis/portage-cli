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
//! `cross.active` (any offset/dual-root context — plain `--root`/`--prefix`
//! included, not just cross-arch) can, for some invocations (e.g. crossdev's
//! own host-arch tools), still have the solver emit its own `MergeRoot::Host`
//! nodes directly into `target_order` — the invariant above doesn't hold
//! universally. This walk must never re-derive or duplicate those: it seeds
//! its availability and seen-set from whatever `MergeRoot::Host` entries are
//! already present, and only fills genuine remaining gaps
//! (`todo/root-topology-refactor.md`, the `dev-perl/Digest-HMAC` duplicate
//! finding).
//!
//! Host copies reuse the Target solve's version for a CPN (same arch, same
//! repo), falling back to the newest *accepted* (keyword/mask/license) repo
//! version for build-only deps absent from the Target plan. The walk is
//! bounded by the host's own availability (VDB, weaving in the `--prefix`
//! VDB the same way `bdepend_avail::Avail::initial_bdepend` does), which
//! already provides the toolchain (gcc, python, …) — so only the genuinely
//! missing libraries are scheduled, and each is emitted only after its own
//! unmet build edges (deps-first, not BFS discovery order) — matching the
//! no-forward-reference invariant `preflight::check` validates for the rest
//! of the plan.

use std::collections::{HashMap, HashSet};

use portage_atom::{Cpn, Cpv, DepEntry, Version};
use portage_atom_pubgrub::{MergeRoot, PortagePackage};

use crate::bdepend_avail::{Avail, unsatisfied_cpns};
use crate::cli::Roots;

use super::effective_use;
use super::repo::{self, Adapter};
use super::root_aware::CrossContext;

/// Static inputs shared across the walk.
struct Ctx<'a> {
    adapter: &'a Adapter<'a>,
    target_ver: &'a HashMap<Cpn, (Version, PortagePackage)>,
}

/// Mutable walk state: host availability (VDB + already-planned Host entries
/// + emitted copies) and the seen-set (also breaks dependency cycles).
struct Walk {
    avail: Avail,
    seen: HashSet<Cpn>,
}

/// Compute the host (`MergeRoot::Host`) build-copies for a native offset
/// (`--root`/`--prefix`, same arch) from the finalized Target `order`.
///
/// Returns `[]` for non-native-offset builds (plain native, cross-arch, host).
pub fn compute(
    target_order: &[(PortagePackage, Version)],
    adapter: &Adapter<'_>,
    roots: &Roots,
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
        adapter,
        target_ver: &target_ver,
    };

    // Host availability starts as the host's own BDEPEND view — the host VDB,
    // plus the prefix's under `--prefix` (same weave `preflight` relies on) —
    // and grows with each host copy.
    let mut walk = Walk {
        seen: HashSet::new(),
        avail: Avail::initial_bdepend(roots),
    };

    // Seed with whatever `MergeRoot::Host` entries the solver already put in
    // `target_order` (e.g. crossdev's own host-arch tools): never re-derive
    // or duplicate those, just record them as already available.
    for (pkg, ver) in target_order
        .iter()
        .filter(|(p, _)| p.merge_root() == MergeRoot::Host)
    {
        walk.seen.insert(*pkg.cpn());
        walk.avail
            .record_merge_bdepend(Cpv::new(*pkg.cpn(), ver.clone()));
    }

    let mut copies: Vec<(PortagePackage, Version)> = Vec::new();
    for (pkg, ver) in target_order
        .iter()
        .filter(|(p, _)| p.merge_root() == MergeRoot::Target)
    {
        visit_unsatisfied(&ctx, &mut walk, pkg, ver, &mut copies);
    }
    copies
}

/// Recurse into `pkg`'s unsatisfied-on-host `DEPEND`/`BDEPEND`/`IDEPEND`
/// edges, appending each resolved host copy to `copies` only *after* its own
/// edges have been visited — deps-first, so a later preflight scan never
/// sees a copy positioned before something it needs.
fn visit_unsatisfied(
    ctx: &Ctx<'_>,
    walk: &mut Walk,
    pkg: &PortagePackage,
    ver: &Version,
    copies: &mut Vec<(PortagePackage, Version)>,
) {
    let Some(cache) = repo::find_cache(ctx.adapter.data, pkg, ver) else {
        return;
    };
    let effective = effective_use::effective_use(
        ctx.adapter.use_config,
        ctx.adapter.package_use,
        pkg,
        ver,
        cache,
    );
    for class in [
        &cache.metadata.depend,
        &cache.metadata.bdepend,
        &cache.metadata.idepend,
    ] {
        let entries = DepEntry::evaluate_use(class, &effective);
        for cpn in unsatisfied_cpns(&entries, &walk.avail) {
            if !walk.seen.insert(cpn) {
                continue;
            }
            let Some((cver, cpkg)) = resolve(cpn, ctx) else {
                continue;
            };
            let host_pkg = cpkg.at_merge_root(MergeRoot::Host);
            visit_unsatisfied(ctx, walk, &host_pkg, &cver, copies);
            walk.avail.record_merge_bdepend(Cpv::new(cpn, cver.clone()));
            copies.push((host_pkg, cver));
        }
    }
}

/// Resolve `(version, package)` for a host copy of `cpn`: the Target plan's
/// version when the CPN is also built for Target, else the newest
/// keyword/mask/license-accepted repo version. `None` when the CPN is absent
/// from the repo or has no accepted version.
fn resolve(cpn: Cpn, ctx: &Ctx<'_>) -> Option<(Version, PortagePackage)> {
    if let Some((v, p)) = ctx.target_ver.get(&cpn) {
        return Some((v.clone(), p.clone()));
    }
    let versions = ctx.adapter.data.versions.get(&cpn)?;
    let (cpv, cache) = versions
        .iter()
        .filter(|(cpv, cache)| ctx.adapter.version_accepted(cpv, cache))
        .max_by(|a, b| a.0.version.cmp(&b.0.version))?;
    let slot = &cache.metadata.slot.slot;
    let pkg = if slot.as_str() == "0" {
        PortagePackage::unslotted(cpn)
    } else {
        PortagePackage::slotted(cpn, *slot)
    };
    Some((cpv.version.clone(), pkg))
}
