//! Effective per-package USE after profile/env overrides and IUSE defaults.

use std::collections::HashMap;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpv, Dep, Version};
use portage_atom_pubgrub::{IUseDefault, PortagePackage, UseConfig, UseOverride};
use portage_metadata::{CacheEntry, IUseDefault as MetaIUseDefault};

pub(super) fn iuse_defaults(cache: &CacheEntry) -> HashMap<Interned<DefaultInterner>, IUseDefault> {
    cache
        .metadata
        .iuse
        .iter()
        .filter_map(|iuse| {
            iuse.default.map(|def| {
                (
                    iuse.interned(),
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
    use_config: &UseConfig,
    package_use: &[(Dep, Vec<UseOverride>)],
    pkg: &PortagePackage,
    ver: &Version,
    cache: &CacheEntry,
) -> UseConfig {
    let cpv = Cpv::new(*pkg.cpn(), ver.clone());
    let mut cfg =
        portage_atom_pubgrub::apply_package_use(use_config, &cpv, pkg.slot(), package_use)
            .into_owned();
    cfg.fold_iuse_defaults(&iuse_defaults(cache));
    cfg
}
