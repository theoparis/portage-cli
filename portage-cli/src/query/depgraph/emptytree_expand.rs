//! Native `--emptytree` deep-closure expansion (Tier C).
//!
//! # Problem
//!
//! Under `emerge -pe`, emerge applies per-root **satisfaction** during the solve
//! (host BROOT for `BDEPEND`/`IDEPEND`, target ROOT for `DEPEND`/`RDEPEND`/`PDEPEND`)
//! but still **lists** every package in the deep closure as `[ebuild R]` / `U` lines.
//! Our solver drops satisfied edges via `broot_filter` and target-VDB shortcuts; this
//! pass re-adds the best matching **repository** CPV for each missing atom the
//! installed tree already satisfies.
//!
//! # Portage parity model (do not confuse with category quirks)
//!
//! Emerge's `-pe` recursion is **category-agnostic**: `recurse_satisfied` in
//! `depgraph.py` walks satisfied edges for *any* parent in the graph when `deep`
//! is active — not only `virtual/*` or `app-alternatives/*`.
//!
//! Portage *does* treat those categories specially during **resolution** (virtual
//! provider expansion, alternatives USE selection, slot lookahead). That is unrelated
//! to which dep *strings* must be read when re-listing satisfied rebuilds here.
//!
//! ## Two different "virtual" concepts
//!
//! | Name | What it is | Expand pass |
//! |------|------------|-------------|
//! | [`PortagePackage::is_virtual`] | Solver-internal Choice / UseDecision nodes | **Skip** — no ebuild metadata |
//! | `virtual/*` **ebuilds** | Real repo packages; usually RDEPEND-only indirection | **Walk** like any ebuild |
//!
//! ## Dep classes → satisfaction root (PMS table 8.2)
//!
//! Every real ebuild in the plan contributes **all five** dep fields below.
//! Empty fields are harmless (`virtual/pkgconfig` has only `RDEPEND`).
//!
//! | Dep class | Satisfy against before re-adding |
//! |-----------|-------------------------------------|
//! | `BDEPEND`, `IDEPEND` | BROOT ([`Avail::initial_bdepend`]) |
//! | `DEPEND`, `RDEPEND`, `PDEPEND` | ROOT ([`Avail::initial_depend`]) |
//!
//! ## Pitfall that caused the List-MoreUtils gap
//!
//! Do **not** limit `RDEPEND`/`PDEPEND` collection to `virtual/*` or
//! `app-alternatives/*`. Build-only tools (e.g. `dev-perl/XS-Parse-Keyword`
//! `RDEPEND` → `File-ShareDir`) are ordinary ebuilds; emerge lists their satisfied
//! runtime edges under `-pe` the same way.
//!
//! Regression anchor: `firefox -pe` should include `List-MoreUtils-XS` when emerge
//! does (~396/400 CPV shared on a typical host).
//!
//! ## Pitfall: `||` groups
//!
//! When collecting atoms for re-listing, walk only the **first satisfied** branch of
//! `AnyOf` (portage picks one `||` alternative, not every branch that happens to be
//! installed). `AllOf` is walked only when every child is satisfied.

use std::collections::HashSet;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, DepEntry, Version};
use portage_atom_pubgrub::{PortagePackage, UseConfig};
use portage_metadata::CacheEntry;

use gentoo_core::Arch;
use portage_repo::AcceptLicense;

use crate::bdepend_avail::{Avail, entry_satisfied};
use crate::cli::Roots;

use super::repo::{self, RepoData, is_masked, keyword_accepts, license_accepted};

/// Inputs for post-solve emptytree expansion.
pub struct ExpandCtx<'a> {
    pub roots: &'a Roots,
    pub data: &'a RepoData,
    pub arch: &'a Arch,
    pub accept_keywords: &'a [String],
    pub package_mask: &'a [portage_atom::Dep],
    pub package_unmask: &'a [portage_atom::Dep],
    pub accept_license: &'a AcceptLicense,
    pub use_config: &'a UseConfig,
    pub package_use: &'a [(portage_atom::Dep, Vec<String>)],
}

/// Re-add satisfied deep-closure packages dropped during the emptytree solve.
pub fn expand_satisfied_rebuilds(
    order: Vec<(PortagePackage, Version)>,
    ctx: &ExpandCtx<'_>,
) -> Vec<(PortagePackage, Version)> {
    if order.is_empty() {
        return order;
    }

    let host = Avail::initial_bdepend(ctx.roots);
    let target = Avail::initial_depend(ctx.roots);
    let mut order = order;
    loop {
        let deps = collect_plan_dep_atoms(&order, ctx, &host, &target);
        let mut prepend: Vec<(PortagePackage, Version)> = Vec::new();
        let mut seen: HashSet<Cpv> = order
            .iter()
            .map(|(p, v)| Cpv::new(*p.cpn(), v.clone()))
            .collect();
        for (dep, root) in deps {
            if plan_satisfies_dep(&order, &dep) {
                continue;
            }
            let installed = match root {
                SatisfyRoot::Broot => &host,
                SatisfyRoot::Target => &target,
            };
            if !installed.atom_satisfied(&dep) {
                continue;
            }
            let Some((pkg, ver)) = best_matching_version(ctx, &dep) else {
                continue;
            };
            let cpv = Cpv::new(*pkg.cpn(), ver.clone());
            if !seen.insert(cpv) {
                continue;
            }
            prepend.push((pkg, ver));
        }
        if prepend.is_empty() {
            break;
        }
        prepend.extend(order);
        order = prepend;
    }

    order = drop_superseded_build_deps(order, ctx, &host, &target);
    // Idempotent when the solver already picked source `dev-lang/rust`.
    order = trim_rust_bin_when_source_present(order);
    order
}

/// Which installed view must satisfy an edge before we re-add it as a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SatisfyRoot {
    /// BROOT — `BDEPEND` / `IDEPEND`.
    Broot,
    /// ROOT / SYSROOT — `DEPEND`, `RDEPEND`, `PDEPEND`.
    Target,
}

/// Dep metadata fields walked for every real ebuild, with their satisfaction root.
const DEP_CLASS_WALKS: &[(&str, SatisfyRoot)] = &[
    ("BDEPEND", SatisfyRoot::Broot),
    ("IDEPEND", SatisfyRoot::Broot),
    ("DEPEND", SatisfyRoot::Target),
    ("RDEPEND", SatisfyRoot::Target),
    ("PDEPEND", SatisfyRoot::Target),
];

fn dep_entries_for_class<'a>(cache: &'a CacheEntry, class: &str) -> &'a [DepEntry] {
    match class {
        "BDEPEND" => &cache.metadata.bdepend,
        "IDEPEND" => &cache.metadata.idepend,
        "DEPEND" => &cache.metadata.depend,
        "RDEPEND" => &cache.metadata.rdepend,
        "PDEPEND" => &cache.metadata.pdepend,
        _ => &[],
    }
}

/// Collect USE-evaluated atoms from every real ebuild already in the plan.
fn collect_plan_dep_atoms(
    order: &[(PortagePackage, Version)],
    ctx: &ExpandCtx<'_>,
    host: &Avail,
    target: &Avail,
) -> Vec<(Dep, SatisfyRoot)> {
    let mut out: Vec<(Dep, SatisfyRoot)> = Vec::new();
    for (pkg, ver) in order {
        // Solver-internal virtual nodes have no md5-cache entry.
        if pkg.is_virtual() {
            continue;
        }
        let Some(cache) = repo::find_cache(ctx.data, pkg, ver) else {
            continue;
        };
        let active = active_flags(pkg, ver, ctx);
        let is_active = |f: &str| active.contains(f);
        for &(class, root) in DEP_CLASS_WALKS {
            let entries = DepEntry::evaluate_use(dep_entries_for_class(cache, class), is_active);
            let avail = match root {
                SatisfyRoot::Broot => &host,
                SatisfyRoot::Target => &target,
            };
            collect_atoms_from_entries(&entries, root, avail, &mut out);
        }
    }
    out
}

fn collect_atoms_from_entries(
    entries: &[DepEntry],
    root: SatisfyRoot,
    avail: &Avail,
    out: &mut Vec<(Dep, SatisfyRoot)>,
) {
    for e in entries {
        match e {
            DepEntry::Atom(dep) if dep.blocker.is_none() => out.push((dep.clone(), root)),
            DepEntry::AllOf(c) if c.iter().all(|child| entry_satisfied(child, avail)) => {
                collect_atoms_from_entries(c, root, avail, out);
            }
            // Emerge lists one satisfied alternative, not every installed branch.
            DepEntry::AnyOf(c) | DepEntry::ExactlyOneOf(c) | DepEntry::AtMostOneOf(c) => {
                for child in c {
                    if entry_satisfied(child, avail) {
                        collect_atoms_from_entries(std::slice::from_ref(child), root, avail, out);
                        break;
                    }
                }
            }
            _ => {}
        }
    }
}

fn active_flags(pkg: &PortagePackage, ver: &Version, ctx: &ExpandCtx<'_>) -> HashSet<String> {
    let cpv = Cpv::new(*pkg.cpn(), ver.clone());
    let effective =
        portage_atom_pubgrub::apply_package_use(ctx.use_config, &cpv, pkg.slot(), ctx.package_use);
    let mut flags: HashSet<String> = effective
        .enabled_flags()
        .iter()
        .map(|f| f.as_str().to_string())
        .collect();
    if let Some(cache) = repo::find_cache(ctx.data, pkg, ver) {
        for iuse in &cache.metadata.iuse {
            if iuse.is_enabled_default()
                && effective.get_opt(&Interned::intern(iuse.name())).is_none()
            {
                flags.insert(iuse.name().to_string());
            }
        }
    }
    flags
}

fn plan_satisfies_dep(order: &[(PortagePackage, Version)], dep: &Dep) -> bool {
    order.iter().any(|(pkg, ver)| {
        let slot = pkg.slot();
        dep.matches_cpv(
            &Cpv::new(*pkg.cpn(), ver.clone()),
            slot.as_ref().map(|s| s.as_str()),
        )
    })
}

fn version_accepted(ctx: &ExpandCtx<'_>, cpv: &Cpv, cache: &portage_metadata::CacheEntry) -> bool {
    let meta = &cache.metadata;
    keyword_accepts(&meta.keywords, ctx.arch.as_str(), ctx.accept_keywords)
        && !is_masked(ctx.package_mask, ctx.package_unmask, cpv, &meta.slot)
        && meta
            .license
            .as_ref()
            .is_none_or(|lic| license_accepted(lic, ctx.accept_license))
        && !meta
            .properties
            .iter()
            .any(|pr| matches!(pr, portage_metadata::RestrictExpr::Token(t) if t == "live"))
}

/// Newest accepted repository CPV matching `dep` (upgrade when the host satisfied an older build).
fn best_matching_version(ctx: &ExpandCtx<'_>, dep: &Dep) -> Option<(PortagePackage, Version)> {
    let entries = ctx.data.versions.get(&dep.cpn)?;
    let mut best: Option<(Cpv, Interned<DefaultInterner>)> = None;
    for (cpv, cache) in entries {
        if !version_accepted(ctx, cpv, cache) {
            continue;
        }
        let slot = cache.metadata.slot.slot;
        let slot_opt = if slot.as_str().is_empty() {
            None
        } else {
            Some(slot.as_str())
        };
        if !dep.matches_cpv(cpv, slot_opt) {
            continue;
        }
        if best.as_ref().is_none_or(|(b, _)| cpv.version > b.version) {
            best = Some((cpv.clone(), slot));
        }
    }
    let (cpv, slot) = best?;
    let pkg = if slot.as_str().is_empty() {
        PortagePackage::unslotted(cpv.cpn)
    } else {
        PortagePackage::slotted(cpv.cpn, slot)
    };
    Some((pkg, cpv.version))
}

fn drop_superseded_build_deps(
    order: Vec<(PortagePackage, Version)>,
    ctx: &ExpandCtx<'_>,
    host: &Avail,
    target: &Avail,
) -> Vec<(PortagePackage, Version)> {
    let deps: Vec<Dep> = collect_plan_dep_atoms(&order, ctx, host, target)
        .into_iter()
        .map(|(d, _)| d)
        .collect();
    let snapshot = order.clone();
    order
        .into_iter()
        .filter(|(pkg, ver)| !is_superseded(pkg, ver, &snapshot, &deps))
        .collect()
}

fn is_superseded(
    pkg: &PortagePackage,
    ver: &Version,
    order: &[(PortagePackage, Version)],
    deps: &[Dep],
) -> bool {
    let cpv = Cpv::new(*pkg.cpn(), ver.clone());
    let slot = pkg.slot();
    let matching: Vec<&Dep> = deps
        .iter()
        .filter(|d| d.matches_cpv(&cpv, slot.as_ref().map(|s| s.as_str())))
        .collect();
    if matching.is_empty() {
        return false;
    }
    order.iter().any(|(other_pkg, other_ver)| {
        if other_pkg.cpn() != pkg.cpn() || other_ver <= ver {
            return false;
        }
        let other_cpv = Cpv::new(*other_pkg.cpn(), other_ver.clone());
        let other_slot = other_pkg.slot();
        matching
            .iter()
            .all(|d| d.matches_cpv(&other_cpv, other_slot.as_ref().map(|s| s.as_str())))
    })
}

/// Emerge builds `dev-lang/rust` from source; drop stale `rust-bin` slots when source is planned.
fn trim_rust_bin_when_source_present(
    order: Vec<(PortagePackage, Version)>,
) -> Vec<(PortagePackage, Version)> {
    let rust = match Cpn::parse("dev-lang/rust") {
        Ok(c) => c,
        Err(_) => return order,
    };
    let rust_bin = match Cpn::parse("dev-lang/rust-bin") {
        Ok(c) => c,
        Err(_) => return order,
    };
    if !order.iter().any(|(p, _)| p.cpn() == &rust) {
        return order;
    }
    order
        .into_iter()
        .filter(|(p, _)| p.cpn() != &rust_bin)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dep_class_walks_cover_all_pms_build_fields() {
        let classes: Vec<&str> = DEP_CLASS_WALKS.iter().map(|(c, _)| *c).collect();
        assert_eq!(
            classes,
            ["BDEPEND", "IDEPEND", "DEPEND", "RDEPEND", "PDEPEND"]
        );
    }

    #[test]
    fn broot_vs_target_roots_match_pms_table() {
        for &(class, root) in DEP_CLASS_WALKS {
            let expect_broot = matches!(class, "BDEPEND" | "IDEPEND");
            assert_eq!(
                root == SatisfyRoot::Broot,
                expect_broot,
                "{class} root mismatch"
            );
        }
    }

    #[test]
    fn any_of_collects_first_satisfied_branch_only() {
        let glibc = Cpn::parse("sys-libs/glibc").unwrap();
        let libbsd = Cpn::parse("dev-libs/libbsd").unwrap();
        let entries = DepEntry::parse("|| ( >=sys-libs/glibc-2.36 dev-libs/libbsd )").unwrap();

        let glibc_only = Avail::from_cpvs(vec![(
            Cpv::new(glibc, Version::parse("2.43-r2").unwrap()),
            None,
        )]);
        let mut out = Vec::new();
        collect_atoms_from_entries(&entries, SatisfyRoot::Target, &glibc_only, &mut out);
        assert!(out.iter().any(|(d, _)| d.cpn == glibc));
        assert!(!out.iter().any(|(d, _)| d.cpn == libbsd));

        let both = Avail::from_cpvs(vec![
            (Cpv::new(glibc, Version::parse("2.43-r2").unwrap()), None),
            (Cpv::new(libbsd, Version::parse("0.11.8").unwrap()), None),
        ]);
        let mut out = Vec::new();
        collect_atoms_from_entries(&entries, SatisfyRoot::Target, &both, &mut out);
        assert!(out.iter().any(|(d, _)| d.cpn == glibc));
        assert!(
            !out.iter().any(|(d, _)| d.cpn == libbsd),
            "leftmost satisfied || branch wins; do not fan out into libbsd"
        );
    }
}
