use std::collections::{HashMap, HashSet};

use gentoo_core::Arch;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{
    IUseDefault, PackageDeps, PackageRepository, PackageVersions, PortagePackage,
};
use portage_metadata::{CacheEntry, Keyword, Stability};
use portage_repo::{CacheReadOpts, Repository, cache_entries_parallel};

pub(super) fn keyword_accepts(keywords: &[Keyword], arch: &str) -> bool {
    keywords.iter().any(|kw| {
        kw.arch.as_str() == arch && matches!(kw.stability, Stability::Stable | Stability::Testing)
    })
}

pub(super) struct RepoData {
    pub(super) cpns: Vec<Cpn>,
    pub(super) versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>>,
    pub(super) repo_name: String,
}

pub(super) struct Adapter<'a> {
    pub(super) data: &'a RepoData,
    pub(super) arch: &'a Arch,
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
                    .filter(|(_, cache)| {
                        keyword_accepts(&cache.metadata.keywords, self.arch.as_str())
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

/// Map a dep atom to a `PortagePackage` for the solver, selecting the slot
/// that contains the highest arch-compatible version when multiple slots exist.
pub(super) fn target_package(data: &RepoData, dep: &Dep, arch: &Arch) -> PortagePackage {
    let entries = match data.versions.get(&dep.cpn) {
        Some(e) => e,
        None => return PortagePackage::unslotted(dep.cpn),
    };

    let arch_entries: Vec<_> = entries
        .iter()
        .filter(|(_, cache)| keyword_accepts(&cache.metadata.keywords, arch.as_str()))
        .collect();

    if arch_entries.is_empty() {
        return PortagePackage::unslotted(dep.cpn);
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
        [] => PortagePackage::unslotted(dep.cpn),
        [sole] => PortagePackage::slotted(dep.cpn, *sole),
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
            PortagePackage::slotted(dep.cpn, best)
        }
    }
}

pub(super) fn find_cache<'a>(
    data: &'a RepoData,
    pkg: &PortagePackage,
    ver: &Version,
) -> Option<&'a CacheEntry> {
    data.versions
        .get(pkg.cpn())?
        .iter()
        .find(|(cpv, _)| &cpv.version == ver)
        .map(|(_, e)| e)
}
