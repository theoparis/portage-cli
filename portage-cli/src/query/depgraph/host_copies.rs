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

use portage_atom::{Cpn, Cpv, Version};
use portage_atom_pubgrub::{MergeRoot, PortagePackage};

use crate::bdepend_avail::{Avail, unsatisfied_cpns};
use crate::cli::Roots;

use super::effective_use;
use super::repo::Adapter;
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
        visit_unsatisfied(&ctx, &mut walk, pkg, ver, &mut copies, true);
    }
    copies
}

/// Recurse into `pkg`'s unsatisfied-on-host `DEPEND`/`BDEPEND`/`IDEPEND`
/// edges, appending each resolved host copy to `copies` only *after* its own
/// edges have been visited — deps-first, so a later preflight scan never
/// sees a copy positioned before something it needs.
///
/// `top_level` is `true` only for the direct per-Target-package calls from
/// [`compute`], not for edges discovered by recursing into an already-found
/// copy's own edges. Under `cross.active`, the solver's own dual-root
/// expansion (`append_unsatisfied_broot` in `portage-atom-pubgrub`) should
/// already schedule a `MergeRoot::Host` node for every built Target
/// package's unsatisfied BDEPEND/IDEPEND edge — so a *top-level* BDEPEND/
/// IDEPEND gap reaching here means this walk's `Avail` view and the
/// solver's own `host_installed` view disagreed, or a post-solve trim
/// dropped a needed `@host` entry: worth surfacing, see
/// `todo/dedup-availability-walks.md` Step 4. A copy's own recursed-into
/// edges never trigger this: those packages never went through the solver,
/// nothing else schedules their build deps.
fn visit_unsatisfied(
    ctx: &Ctx<'_>,
    walk: &mut Walk,
    pkg: &PortagePackage,
    ver: &Version,
    copies: &mut Vec<(PortagePackage, Version)>,
    top_level: bool,
) {
    let Some(deps) = effective_use::evaluated_deps(
        ctx.adapter.data,
        ctx.adapter.use_config,
        ctx.adapter.package_use,
        pkg,
        ver,
    ) else {
        return;
    };
    for (class, entries) in [
        ("DEPEND", deps.depend()),
        ("BDEPEND", deps.bdepend()),
        ("IDEPEND", deps.idepend()),
    ] {
        for cpn in unsatisfied_cpns(&entries, &walk.avail) {
            if !walk.seen.insert(cpn) {
                continue;
            }
            if top_level && class != "DEPEND" {
                eprintln!(
                    "!!! host_copies: top-level {class} gap for {cpn} (from {pkg}) — \
                     the solver should already cover this under cross.active; \
                     see todo/dedup-availability-walks.md Step 4"
                );
            }
            let Some((cver, cpkg)) = resolve(cpn, ctx) else {
                continue;
            };
            let host_pkg = cpkg.at_merge_root(MergeRoot::Host);
            visit_unsatisfied(ctx, walk, &host_pkg, &cver, copies, false);
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
    let (cpv, cache) = ctx.adapter.newest_accepted(cpn)?;
    let slot = &cache.metadata.slot.slot;
    let pkg = if slot.as_str() == "0" {
        PortagePackage::unslotted(cpn)
    } else {
        PortagePackage::slotted(cpn, *slot)
    };
    Some((cpv.version.clone(), pkg))
}

#[cfg(test)]
mod tests {
    use portage_atom_pubgrub::UseConfig;
    use portage_metadata::CacheEntry;
    use portage_repo::{AcceptLicense, LicenseGroupRegistry};

    use super::super::force_mask::ForceMask;
    use super::super::repo::{self, AcceptKeywords, AcceptLicenses};
    use super::super::root_aware;
    use super::*;

    fn accept_all_licenses() -> AcceptLicense {
        AcceptLicense::from_tokens(&["*".into()], &LicenseGroupRegistry::default())
    }

    /// Build a `RepoData` from `(cpv, md5-cache-text)` pairs, one version per CPN
    /// (mirrors the same-shaped helper in `bdepend_trim`'s and `depend_trim`'s
    /// own test modules).
    fn repo_from(entries: &[(&str, &str)]) -> repo::RepoData {
        let mut versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
        let mut cpns = Vec::new();
        for (cpv_str, text) in entries {
            let cpv = Cpv::parse(cpv_str).unwrap();
            let entry = CacheEntry::parse(text).unwrap();
            cpns.push(cpv.cpn);
            versions.entry(cpv.cpn).or_default().push((cpv, entry));
        }
        repo::RepoData {
            cpns,
            versions,
            repo_name: "test".into(),
            repo_of: HashMap::new(),
            real_cpn_of: HashMap::new(),
        }
    }

    /// A `--prefix`-shaped `CrossContext`: `active` (target != host), not
    /// cross-arch (no make.conf to read a foreign `CHOST` from) — the native
    /// offset case `host_copies::compute` exists for.
    fn native_offset_cross(roots: &Roots) -> CrossContext {
        root_aware::detect(roots, roots.merge_root())
    }

    /// Regression test for the `5989eb1` fix (the `dev-perl/Digest-HMAC`
    /// duplicate-plan-entry incident, `todo/root-topology-refactor.md`): when
    /// the solver's own dual-root expansion has already scheduled a
    /// `MergeRoot::Host` node for a CPN (simulating crossdev's host-arch
    /// tools), `compute` must not re-derive or duplicate it. Before that fix
    /// this produced a second, independently-versioned, anti-topologically
    /// ordered copy; this pins the seeding behavior that stops it, which had
    /// no test of its own — the fix was verified live only.
    #[test]
    fn does_not_duplicate_a_solver_seeded_host_entry() {
        let data = repo_from(&[
            (
                "sys-apps/consumer-1.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\nBDEPEND=dev-libs/tool\n",
            ),
            (
                "dev-libs/tool-2.0",
                "EAPI=8\nSLOT=0\nKEYWORDS=amd64\nDESCRIPTION=t\n",
            ),
        ]);

        let consumer = (
            PortagePackage::unslotted(Cpn::parse("sys-apps/consumer").unwrap()),
            Version::parse("1.0").unwrap(),
        );
        // Stands in for the solver's own `append_unsatisfied_broot` output:
        // this CPN's BDEPEND is already scheduled `@host`.
        let tool_host = (
            PortagePackage::unslotted(Cpn::parse("dev-libs/tool").unwrap())
                .at_merge_root(MergeRoot::Host),
            Version::parse("2.0").unwrap(),
        );
        let target_order = vec![consumer, tool_host];

        let arch = gentoo_core::Arch::intern("amd64");
        let accept_keywords = AcceptKeywords::from_global(&arch, &["amd64"]);
        let use_config = UseConfig::new();
        let force_mask = ForceMask::default();
        let installed_cpvs = std::collections::HashSet::new();
        let adapter = Adapter {
            data: &data,
            accept_keywords: &accept_keywords,
            package_mask: &[],
            package_unmask: &[],
            accept_licenses: &AcceptLicenses::new(accept_all_licenses(), Vec::new()),
            use_config: &use_config,
            package_use: &[],
            force_mask: &force_mask,
            installed_cpvs: &installed_cpvs,
            autosolve_use: false,
        };

        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        let roots = Roots::for_test_overlay(
            host.path().to_str().unwrap(),
            prefix.path().to_str().unwrap(),
        );
        let cross = native_offset_cross(&roots);
        assert!(
            cross.active && !cross.is_cross_arch(),
            "test setup must land in the native-offset case compute() exists for"
        );

        let copies = compute(&target_order, &adapter, &roots, &cross);
        assert!(
            copies.is_empty(),
            "must not re-derive a CPN the solver already scheduled @host: {copies:?}"
        );
    }
}
