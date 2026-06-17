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
//!
//! ## Pitfall: expand-added build deps (Group A)
//!
//! Packages re-added because BROOT already satisfies their atom (e.g. `firefox`
//! `BDEPEND` → `nodejs`) still need their own `DEPEND`/`BDEPEND` closure scheduled on
//! the correct root. Only re-listing **satisfied** atoms misses upgrades like
//! `nodejs` → `>=dev-libs/simdjson-4.6.1` when the VDB still has an older slot.
//!
//! ## Pitfall: slot-agnostic atoms and `||` preference (Group B)
//!
//! Re-listing must target the same CPV emerge would add: `plan_satisfies_dep` is too
//! coarse when an atom omits `:slot` but the newest installed match is slot 22 while
//! the plan only has slot 21 (`clang-common` → `clang-runtime`). For satisfied `||`
//! groups with both `dev-lang/rust-bin` and `dev-lang/rust` branches, emerge prefers
//! source `rust` (dep_zapdeps), not the leftmost `rust-bin` branch.

use std::borrow::Cow;
use std::collections::HashSet;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, DepEntry, Version};
use portage_atom_pubgrub::{IUseDefault, PortagePackage, UseConfig, UseFlagState};
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
        let satisfied_deps = collect_plan_dep_atoms(&order, ctx, &host, &target);
        let build_deps = collect_plan_build_dep_atoms(&order, ctx, &host, &target);
        let mut prepend: Vec<(PortagePackage, Version)> = Vec::new();
        let mut seen: HashSet<Cpv> = order
            .iter()
            .map(|(p, v)| Cpv::new(*p.cpn(), v.clone()))
            .collect();
        for (dep, root) in satisfied_deps {
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
        for (dep, root) in build_deps {
            if plan_satisfies_dep(&order, &dep) {
                continue;
            }
            let installed = match root {
                SatisfyRoot::Broot => &host,
                SatisfyRoot::Target => &target,
            };
            if installed.atom_satisfied(&dep) {
                continue;
            }
            let _ = prepend_candidate(ctx, &dep, &mut seen, &mut prepend);
        }
        if prepend.is_empty() {
            break;
        }
        prepend.extend(order);
        order = prepend;
    }

    order = drop_superseded_build_deps(order, ctx, &host, &target);
    order = trim_rust_bin_when_source_present(order);
    order = trim_superseded_rust_sources(order, ctx);
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

/// Build-time fields whose unsatisfied atoms must be scheduled when a parent lands in
/// the plan via expand (not only re-listed when already satisfied on that root).
const DEP_CLASS_BUILD: &[(&str, SatisfyRoot)] = &[
    ("BDEPEND", SatisfyRoot::Broot),
    ("IDEPEND", SatisfyRoot::Broot),
    ("DEPEND", SatisfyRoot::Target),
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
        let effective = effective_use(pkg, ver, ctx);
        let is_active = |f: &str| is_flag_active(&effective, Some(cache), f);
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

/// Collect build-time atoms from packages already in the plan, including deps not yet
/// satisfied on the correct root (for expand-added parents like host-satisfied `nodejs`).
fn collect_plan_build_dep_atoms(
    order: &[(PortagePackage, Version)],
    ctx: &ExpandCtx<'_>,
    host: &Avail,
    target: &Avail,
) -> Vec<(Dep, SatisfyRoot)> {
    let mut out: Vec<(Dep, SatisfyRoot)> = Vec::new();
    for (pkg, ver) in order {
        if pkg.is_virtual() {
            continue;
        }
        let Some(cache) = repo::find_cache(ctx.data, pkg, ver) else {
            continue;
        };
        let effective = effective_use(pkg, ver, ctx);
        let is_active = |f: &str| is_flag_active(&effective, Some(cache), f);
        for &(class, root) in DEP_CLASS_BUILD {
            let entries = DepEntry::evaluate_use(dep_entries_for_class(cache, class), is_active);
            let avail = match root {
                SatisfyRoot::Broot => &host,
                SatisfyRoot::Target => &target,
            };
            collect_build_atoms_from_entries(&entries, root, avail, &mut out);
        }
    }
    out
}

fn prepend_candidate(
    ctx: &ExpandCtx<'_>,
    dep: &Dep,
    seen: &mut HashSet<Cpv>,
    prepend: &mut Vec<(PortagePackage, Version)>,
) -> bool {
    let Some((pkg, ver)) = best_matching_version(ctx, dep) else {
        return false;
    };
    let cpv = Cpv::new(*pkg.cpn(), ver.clone());
    if !seen.insert(cpv) {
        return false;
    }
    prepend.push((pkg, ver));
    true
}

/// Pick the `||` branch emerge would recurse into when several are satisfied.
fn pick_satisfied_or_branch<'a>(children: &'a [DepEntry], avail: &Avail) -> Option<&'a DepEntry> {
    let satisfied: Vec<&DepEntry> = children
        .iter()
        .filter(|c| entry_satisfied(c, avail))
        .collect();
    match satisfied.len() {
        0 => None,
        1 => satisfied.first().copied(),
        _ => prefer_or_branch(satisfied),
    }
}

fn prefer_or_branch(branches: Vec<&DepEntry>) -> Option<&DepEntry> {
    let rust = Cpn::parse("dev-lang/rust").ok();
    let rust_bin = Cpn::parse("dev-lang/rust-bin").ok();
    let mut first = None;
    let mut src = None;
    let mut bin = None;
    for branch in branches {
        if first.is_none() {
            first = Some(branch);
        }
        let DepEntry::Atom(dep) = branch else {
            continue;
        };
        if rust.as_ref().is_some_and(|cpn| dep.cpn == *cpn) {
            src = Some(branch);
        }
        if rust_bin.as_ref().is_some_and(|cpn| dep.cpn == *cpn) {
            bin = Some(branch);
        }
    }
    if src.is_some() && bin.is_some() {
        return src;
    }
    first
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
                if let Some(child) = pick_satisfied_or_branch(c, avail) {
                    collect_atoms_from_entries(std::slice::from_ref(child), root, avail, out);
                }
            }
            _ => {}
        }
    }
}

/// Collect atoms required to build parents in the plan. Unlike
/// [`collect_atoms_from_entries`], walks every `AllOf` child and, for `||` groups
/// with no satisfied branch yet, takes the first listed alternative to schedule.
fn collect_build_atoms_from_entries(
    entries: &[DepEntry],
    root: SatisfyRoot,
    avail: &Avail,
    out: &mut Vec<(Dep, SatisfyRoot)>,
) {
    for e in entries {
        match e {
            DepEntry::Atom(dep) if dep.blocker.is_none() => out.push((dep.clone(), root)),
            DepEntry::AllOf(c) => collect_build_atoms_from_entries(c, root, avail, out),
            DepEntry::AnyOf(c) | DepEntry::ExactlyOneOf(c) | DepEntry::AtMostOneOf(c) => {
                let mut picked = false;
                for child in c {
                    if entry_satisfied(child, avail) {
                        collect_build_atoms_from_entries(
                            std::slice::from_ref(child),
                            root,
                            avail,
                            out,
                        );
                        picked = true;
                        break;
                    }
                }
                if !picked && let Some(first) = c.first() {
                    collect_build_atoms_from_entries(std::slice::from_ref(first), root, avail, out);
                }
            }
            _ => {}
        }
    }
}

fn effective_use<'a>(
    pkg: &PortagePackage,
    ver: &Version,
    ctx: &'a ExpandCtx<'_>,
) -> Cow<'a, UseConfig> {
    let cpv = Cpv::new(*pkg.cpn(), ver.clone());
    portage_atom_pubgrub::apply_package_use(ctx.use_config, &cpv, pkg.slot(), ctx.package_use)
}

fn is_flag_active(effective: &UseConfig, cache: Option<&CacheEntry>, flag: &str) -> bool {
    let iuse_default = cache.and_then(|c| {
        c.metadata
            .iuse
            .iter()
            .find(|i| i.name() == flag)
            .and_then(|i| i.default)
            .map(|d| match d {
                portage_metadata::IUseDefault::Enabled => IUseDefault::Enabled,
                portage_metadata::IUseDefault::Disabled => IUseDefault::Disabled,
            })
    });
    matches!(
        effective.get_with_iuse_default(&Interned::intern(flag), iuse_default),
        UseFlagState::Enabled
    )
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
        .chain(collect_plan_build_dep_atoms(&order, ctx, host, target))
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

/// Drop older `dev-lang/rust` per `llvm_slot_*` when expand re-added several satisfied
/// `||` targets; emerge keeps the newest source rust for each LLVM slot.
fn trim_superseded_rust_sources(
    order: Vec<(PortagePackage, Version)>,
    ctx: &ExpandCtx<'_>,
) -> Vec<(PortagePackage, Version)> {
    let rust = match Cpn::parse("dev-lang/rust") {
        Ok(c) => c,
        Err(_) => return order,
    };
    let mut best_per_llvm_slot: std::collections::HashMap<u32, Version> =
        std::collections::HashMap::new();
    for (pkg, ver) in &order {
        if pkg.cpn() != &rust {
            continue;
        }
        let Some(slot) = rust_llvm_slot(pkg, ver, ctx) else {
            continue;
        };
        if best_per_llvm_slot
            .get(&slot)
            .is_none_or(|best| ver > best)
        {
            best_per_llvm_slot.insert(slot, ver.clone());
        }
    }
    if best_per_llvm_slot.is_empty() {
        return order;
    }
    let keep: HashSet<Cpv> = best_per_llvm_slot
        .into_values()
        .map(|ver| Cpv::new(rust, ver))
        .collect();
    order
        .into_iter()
        .filter(|(p, ver)| p.cpn() != &rust || keep.contains(&Cpv::new(*p.cpn(), ver.clone())))
        .collect()
}

fn rust_llvm_slot(pkg: &PortagePackage, ver: &Version, ctx: &ExpandCtx<'_>) -> Option<u32> {
    let cache = repo::find_cache(ctx.data, pkg, ver)?;
    let effective = effective_use(pkg, ver, ctx);
    for iuse in &cache.metadata.iuse {
        let name = iuse.name();
        let Some(n) = name.strip_prefix("llvm_slot_") else {
            continue;
        };
        if is_flag_active(&effective, Some(cache), name) {
            return n.parse().ok();
        }
    }
    None
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

    #[test]
    fn or_prefers_rust_source_over_rust_bin_when_both_satisfied() {
        let rust = Cpn::parse("dev-lang/rust").unwrap();
        let rust_bin = Cpn::parse("dev-lang/rust-bin").unwrap();
        let entries = DepEntry::parse(
            "|| ( >=dev-lang/rust-bin-1.93.0:* >=dev-lang/rust-1.93.0:* )",
        )
        .unwrap();
        let avail = Avail::from_cpvs(vec![
            (Cpv::new(rust_bin, Version::parse("1.93.1").unwrap()), None),
            (Cpv::new(rust, Version::parse("1.95.0").unwrap()), None),
        ]);
        let mut out = Vec::new();
        collect_atoms_from_entries(&entries, SatisfyRoot::Broot, &avail, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.cpn, rust);
    }

    #[test]
    fn build_collects_unsatisfied_atoms() {
        let simdjson = Cpn::parse("dev-libs/simdjson").unwrap();
        let entries = DepEntry::parse(">=dev-libs/simdjson-4.6.1").unwrap();
        let avail = Avail::from_cpvs(vec![(
            Cpv::new(simdjson, Version::parse("4.3.0").unwrap()),
            None,
        )]);
        let mut out = Vec::new();
        collect_build_atoms_from_entries(&entries, SatisfyRoot::Target, &avail, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.cpn, simdjson);
    }

    #[test]
    fn build_any_of_falls_back_to_first_branch_when_unsatisfied() {
        let gcc = Cpn::parse("sys-devel/gcc").unwrap();
        let stub = Cpn::parse("llvm-runtimes/libatomic-stub").unwrap();
        let entries =
            DepEntry::parse("|| ( sys-devel/gcc:* llvm-runtimes/libatomic-stub )").unwrap();
        let avail = Avail::default();
        let mut out = Vec::new();
        collect_build_atoms_from_entries(&entries, SatisfyRoot::Target, &avail, &mut out);
        assert!(out.iter().any(|(d, _)| d.cpn == gcc));
        assert!(!out.iter().any(|(d, _)| d.cpn == stub));
    }
}
