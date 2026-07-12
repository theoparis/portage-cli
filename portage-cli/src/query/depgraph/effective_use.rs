//! Effective per-package USE after profile/env overrides and IUSE defaults.

use std::collections::HashMap;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpv, Dep, DepEntry, Version};
use portage_atom_pubgrub::{IUseDefault, PortagePackage, UseConfig, UseOverride};
use portage_metadata::{CacheEntry, IUseDefault as MetaIUseDefault};

use super::repo::{self, RepoData};

pub(super) fn iuse_defaults(cache: &CacheEntry) -> HashMap<Interned<DefaultInterner>, IUseDefault> {
    cache
        .metadata
        .iuse
        .iter()
        .filter_map(|iuse| {
            iuse.default.map(|def| {
                (
                    iuse.into(),
                    match def {
                        MetaIUseDefault::Enabled => IUseDefault::Enabled,
                        MetaIUseDefault::Disabled => IUseDefault::Disabled,
                    },
                )
            })
        })
        .collect()
}

pub(super) fn effective_use(
    pre_env: &str,
    env_use: &str,
    package_use: &[(Dep, Vec<UseOverride>)],
    pkg: &PortagePackage,
    ver: &Version,
    cache: &CacheEntry,
) -> UseConfig {
    let cpv = Cpv::new(*pkg.cpn(), ver.clone());
    let defaults = iuse_defaults(cache);
    portage_atom_pubgrub::resolve_effective_use(
        &defaults,
        pre_env,
        &cpv,
        pkg.slot(),
        package_use,
        env_use,
    )
}

/// A `(pkg, ver)`'s cache entry plus its effective USE, with each dep class
/// evaluated against that USE on demand — the `find_cache` +
/// [`effective_use`] + `DepEntry::evaluate_use` triple shared by
/// `host_copies`, `bdepend_trim`, and `depend_trim`. `None` when the CPV
/// isn't in `data` at all (a within-run merge whose cache entry vanished,
/// e.g. across a repo reload — every caller already treats this as "skip").
pub(super) struct EvaluatedDeps<'a> {
    cache: &'a CacheEntry,
    effective: UseConfig,
}

impl EvaluatedDeps<'_> {
    pub(super) fn depend(&self) -> Vec<DepEntry> {
        DepEntry::evaluate_use(&self.cache.metadata.depend, &self.effective)
    }

    pub(super) fn bdepend(&self) -> Vec<DepEntry> {
        DepEntry::evaluate_use(&self.cache.metadata.bdepend, &self.effective)
    }

    pub(super) fn rdepend(&self) -> Vec<DepEntry> {
        DepEntry::evaluate_use(&self.cache.metadata.rdepend, &self.effective)
    }

    pub(super) fn pdepend(&self) -> Vec<DepEntry> {
        DepEntry::evaluate_use(&self.cache.metadata.pdepend, &self.effective)
    }

    pub(super) fn idepend(&self) -> Vec<DepEntry> {
        DepEntry::evaluate_use(&self.cache.metadata.idepend, &self.effective)
    }
}

pub(super) fn evaluated_deps<'a>(
    data: &'a RepoData,
    pre_env: &str,
    env_use: &str,
    package_use: &[(Dep, Vec<UseOverride>)],
    pkg: &PortagePackage,
    ver: &Version,
) -> Option<EvaluatedDeps<'a>> {
    let cache = repo::find_cache(data, pkg, ver)?;
    let effective = effective_use(pre_env, env_use, package_use, pkg, ver, cache);
    Some(EvaluatedDeps { cache, effective })
}
