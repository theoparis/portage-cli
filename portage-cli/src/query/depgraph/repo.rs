use std::collections::{HashMap, HashSet};

use gentoo_core::Arch;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Operator, Version};
use portage_atom_pubgrub::{
    DroppedDep, IUseDefault, PackageDeps, PackageRepository, PackageVersions,
};
use portage_metadata::{CacheEntry, Keyword, LicenseExpr, Stability};
use portage_repo::{CacheReadOpts, Repository, cache_entries_parallel};

/// A reason a package version was excluded from the solver.
#[derive(Debug, Clone)]
pub(super) enum FilterReason {
    /// Needs a keyword the system doesn't accept (e.g. `~arm64`).
    Keyword(String),
    /// Masked by the profile or user `package.mask`.
    Masked,
    /// One or more licenses not in ACCEPT_LICENSE.
    License(Vec<String>),
}

/// A package version that was excluded and could resolve a dropped dep.
#[derive(Debug, Clone)]
pub(super) struct AutounmaskCandidate {
    pub cpv: Cpv,
    pub slot: Option<Interned<DefaultInterner>>,
    pub reasons: Vec<FilterReason>,
}

/// Returns true if the keyword list satisfies `accept_keywords` for the given arch.
///
/// Empty `accept_keywords` falls back to accepting stable + testing (tool default).
pub(super) fn keyword_accepts(keywords: &[Keyword], arch: &str, accept_keywords: &[String]) -> bool {
    if accept_keywords.iter().any(|k| k == "**") {
        return true;
    }
    if accept_keywords.is_empty() {
        // No ACCEPT_KEYWORDS loaded; the profile baseline is stable-only.
        return keywords.iter().any(|kw| {
            kw.arch.as_str() == arch && kw.stability == Stability::Stable
        });
    }
    keywords.iter().any(|kw| {
        if kw.arch.as_str() != arch {
            return false;
        }
        let token = match kw.stability {
            Stability::Stable => kw.arch.as_str().to_string(),
            Stability::Testing => format!("~{}", kw.arch.as_str()),
            _ => return false,
        };
        accept_keywords.contains(&token)
    })
}

/// Returns the testing keyword string needed for `arch` if the package only has `~arch`
/// and it is not already in `accept_keywords`.
fn keyword_needed(keywords: &[Keyword], arch: &str, accept_keywords: &[String]) -> Option<String> {
    if keyword_accepts(keywords, arch, accept_keywords) {
        return None;
    }
    // Check whether a testing keyword for this arch exists in the package metadata.
    keywords.iter().find_map(|kw| {
        if kw.arch.as_str() == arch && kw.stability == Stability::Testing {
            Some(format!("~{}", arch))
        } else {
            None
        }
    })
}

/// Returns true if the license expression is fully covered by `accept_license`.
pub(super) fn license_accepted(expr: &LicenseExpr, accept: &[String]) -> bool {
    if accept.iter().any(|a| a == "*") {
        return true;
    }
    match expr {
        LicenseExpr::License(name) => accept.iter().any(|a| a == name),
        LicenseExpr::AnyOf(children) => children.iter().any(|c| license_accepted(c, accept)),
        LicenseExpr::All(children) => children.iter().all(|c| license_accepted(c, accept)),
        LicenseExpr::UseConditional { entries, .. } => {
            entries.iter().all(|e| license_accepted(e, accept))
        }
    }
}

/// Collects the license names that are NOT covered by `accept_license`.
fn licenses_needed(expr: &LicenseExpr, accept: &[String]) -> Vec<String> {
    if accept.iter().any(|a| a == "*") {
        return vec![];
    }
    match expr {
        LicenseExpr::License(name) => {
            if accept.iter().any(|a| a == name) { vec![] } else { vec![name.clone()] }
        }
        LicenseExpr::AnyOf(children) => {
            if children.iter().any(|c| license_accepted(c, accept)) {
                vec![]
            } else {
                children.first().map(|c| licenses_needed(c, accept)).unwrap_or_default()
            }
        }
        LicenseExpr::All(children) => {
            children.iter().flat_map(|c| licenses_needed(c, accept)).collect()
        }
        LicenseExpr::UseConditional { entries, .. } => {
            entries.iter().flat_map(|e| licenses_needed(e, accept)).collect()
        }
    }
}

/// Check whether `mask_dep` matches the given `cpv` (version + CPN, no slot check).
fn mask_matches(mask_dep: &Dep, cpv: &Cpv) -> bool {
    if mask_dep.cpn != cpv.cpn {
        return false;
    }
    let (Some(op), Some(mask_ver)) = (mask_dep.op, &mask_dep.version) else {
        return mask_dep.version.is_none();
    };
    let cand = &cpv.version;
    match op {
        Operator::Equal => {
            if mask_dep.glob { cand.glob_matches(mask_ver) } else { cand == mask_ver }
        }
        Operator::GreaterOrEqual => cand >= mask_ver,
        Operator::Greater => cand > mask_ver,
        Operator::LessOrEqual => cand <= mask_ver,
        Operator::Less => cand < mask_ver,
        Operator::Approximate => {
            let mut base_mask = mask_ver.clone();
            base_mask.revision = Default::default();
            let mut base_cand = cand.clone();
            base_cand.revision = Default::default();
            base_cand == base_mask
        }
    }
}

pub(super) struct RepoData {
    pub(super) cpns: Vec<Cpn>,
    pub(super) versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>>,
    pub(super) repo_name: String,
}

pub(super) struct Adapter<'a> {
    pub(super) data: &'a RepoData,
    pub(super) arch: &'a Arch,
    pub(super) accept_keywords: &'a [String],
    pub(super) package_mask: &'a [Dep],
    pub(super) accept_license: &'a [String],
}

impl PackageRepository for Adapter<'_> {
    fn all_packages(&self) -> Vec<Cpn> {
        self.data.cpns.clone()
    }

    fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, PackageVersions)> {
        self.data
            .versions
            .get(cpn)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(cpv, cache)| {
                        let meta = &cache.metadata;
                        // Keyword check
                        if !keyword_accepts(&meta.keywords, self.arch.as_str(), self.accept_keywords) {
                            return false;
                        }
                        // Mask check
                        if self.package_mask.iter().any(|m| mask_matches(m, cpv)) {
                            return false;
                        }
                        // License check
                        if let Some(lic) = &meta.license {
                            if !license_accepted(lic, self.accept_license) {
                                return false;
                            }
                        }
                        true
                    })
                    .map(|(cpv, cache)| {
                        let meta = &cache.metadata;
                        let slot = if meta.slot.slot.as_str().is_empty() {
                            None
                        } else {
                            Some(meta.slot.slot)
                        };
                        let subslot = meta.slot.subslot;
                        let repo =
                            Some(Interned::<DefaultInterner>::intern(&self.data.repo_name));
                        let iuse: Vec<Interned<DefaultInterner>> = meta
                            .iuse
                            .iter()
                            .map(|iu| Interned::intern(iu.name()))
                            .collect();
                        let iuse_defaults: HashMap<Interned<DefaultInterner>, IUseDefault> = meta
                            .iuse
                            .iter()
                            .filter_map(|iu| {
                                iu.default.map(|d| {
                                    let val = match d {
                                        portage_metadata::IUseDefault::Enabled => {
                                            IUseDefault::Enabled
                                        }
                                        portage_metadata::IUseDefault::Disabled => {
                                            IUseDefault::Disabled
                                        }
                                    };
                                    (Interned::intern(iu.name()), val)
                                })
                            })
                            .collect();
                        let deps = PackageDeps {
                            depend: meta.depend.clone(),
                            rdepend: meta.rdepend.clone(),
                            bdepend: meta.bdepend.clone(),
                            pdepend: meta.pdepend.clone(),
                            idepend: meta.idepend.clone(),
                        };
                        (
                            cpv.clone(),
                            PackageVersions {
                                slot,
                                subslot,
                                repo,
                                iuse,
                                iuse_defaults,
                                deps,
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

pub(super) async fn load_repo(repo: &Repository) -> RepoData {
    let mut cpns_set: HashSet<Cpn> = HashSet::new();
    let mut versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();

    let entries = cache_entries_parallel(
        std::slice::from_ref(repo),
        &CacheReadOpts::default(),
        |text| CacheEntry::parse(text).map_err(portage_repo::Error::from),
    )
    .await;

    for (cpv, entry) in entries {
        if let Ok(entry) = entry {
            let cpn = cpv.cpn;
            cpns_set.insert(cpn);
            versions.entry(cpn).or_default().push((cpv, entry));
        }
    }

    let mut cpns: Vec<Cpn> = cpns_set.into_iter().collect();
    cpns.sort_by_key(|c| format!("{}/{}", c.category, c.package));

    RepoData {
        cpns,
        versions,
        repo_name: repo.name().to_string(),
    }
}

/// Map a dep atom to a `PortagePackage` for the solver.
pub(super) fn target_package(
    data: &RepoData,
    dep: &Dep,
    arch: &Arch,
    accept_keywords: &[String],
    package_mask: &[Dep],
    accept_license: &[String],
) -> portage_atom_pubgrub::PortagePackage {
    let entries = match data.versions.get(&dep.cpn) {
        Some(e) => e,
        None => return portage_atom_pubgrub::PortagePackage::unslotted(dep.cpn),
    };

    let arch_entries: Vec<_> = entries
        .iter()
        .filter(|(cpv, cache)| {
            keyword_accepts(&cache.metadata.keywords, arch.as_str(), accept_keywords)
                && !package_mask.iter().any(|m| mask_matches(m, cpv))
                && cache.metadata.license.as_ref()
                    .map_or(true, |l| license_accepted(l, accept_license))
        })
        .collect();

    if arch_entries.is_empty() {
        return portage_atom_pubgrub::PortagePackage::unslotted(dep.cpn);
    }

    let mut slots: Vec<_> = arch_entries
        .iter()
        .filter_map(|(_, cache)| {
            let s = &cache.metadata.slot.slot;
            if s.as_str().is_empty() { None } else { Some(*s) }
        })
        .collect();
    slots.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    slots.dedup();

    match slots.as_slice() {
        [] => portage_atom_pubgrub::PortagePackage::unslotted(dep.cpn),
        [sole] => portage_atom_pubgrub::PortagePackage::slotted(dep.cpn, *sole),
        _ => {
            let best = arch_entries
                .iter()
                .filter_map(|(cpv, cache)| {
                    let s = &cache.metadata.slot.slot;
                    if s.as_str().is_empty() { None } else { Some((cpv.version.clone(), *s)) }
                })
                .max_by(|a, b| a.0.cmp(&b.0))
                .map(|(_, s)| s)
                .unwrap();
            portage_atom_pubgrub::PortagePackage::slotted(dep.cpn, best)
        }
    }
}

pub(super) fn find_cache<'a>(
    data: &'a RepoData,
    pkg: &portage_atom_pubgrub::PortagePackage,
    ver: &Version,
) -> Option<&'a CacheEntry> {
    data.versions
        .get(pkg.cpn())?
        .iter()
        .find(|(cpv, _)| &cpv.version == ver)
        .map(|(_, e)| e)
}

/// For each dropped dep, find versions in the unfiltered repo that match its
/// version range and determine why they were excluded.
pub(super) fn find_autounmask_candidates(
    data: &RepoData,
    dropped: &[DroppedDep],
    arch: &str,
    accept_keywords: &[String],
    package_mask: &[Dep],
    accept_license: &[String],
) -> Vec<AutounmaskCandidate> {
    let mut candidates = Vec::new();

    for dep in dropped {
        if dep.package.is_virtual() {
            continue;
        }
        let cpn = dep.package.cpn();
        let Some(entries) = data.versions.get(cpn) else {
            continue;
        };

        for (cpv, cache) in entries {
            if !dep.version_set.contains(&cpv.version) {
                continue;
            }
            let meta = &cache.metadata;
            let slot = if meta.slot.slot.as_str().is_empty() {
                None
            } else {
                Some(meta.slot.slot)
            };

            let mut reasons = Vec::new();

            if let Some(kw) = keyword_needed(&meta.keywords, arch, accept_keywords) {
                reasons.push(FilterReason::Keyword(kw));
            }
            if package_mask.iter().any(|m| mask_matches(m, cpv)) {
                reasons.push(FilterReason::Masked);
            }
            if let Some(lic) = &meta.license {
                let needed = licenses_needed(lic, accept_license);
                if !needed.is_empty() {
                    reasons.push(FilterReason::License(needed));
                }
            }

            if !reasons.is_empty() {
                candidates.push(AutounmaskCandidate {
                    cpv: cpv.clone(),
                    slot,
                    reasons,
                });
            }
        }
    }

    candidates
}
