use std::collections::HashMap;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv};

use crate::use_config::UseConfig;

/// Default state for an IUSE flag, from the `+`/`-` prefix in IUSE.
///
/// See [PMS 7.2](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IUseDefault {
    /// `+flag` — enabled by default.
    Enabled,
    /// `-flag` — disabled by default.
    Disabled,
}

/// Metadata for a single ebuild version, including its dependency trees.
#[derive(Clone)]
pub struct PackageVersions {
    /// Slot name for this version.
    pub slot: Option<Interned<DefaultInterner>>,
    /// Subslot name for this version.
    pub subslot: Option<Interned<DefaultInterner>>,
    /// Repository this version comes from.
    pub repo: Option<Interned<DefaultInterner>>,
    /// IUSE flags for this version (USE flags the package defines).
    pub iuse: Vec<Interned<DefaultInterner>>,
    /// IUSE default states — flags prefixed with `+` in IUSE default to enabled.
    pub iuse_defaults: HashMap<Interned<DefaultInterner>, IUseDefault>,
    /// Dependency trees by class.
    pub deps: PackageDeps,
}

/// Structured dependency trees separated by PMS class.
#[derive(Clone)]
pub struct PackageDeps {
    /// DEPEND — build-time dependencies.
    pub depend: Vec<portage_atom::DepEntry>,
    /// RDEPEND — runtime dependencies.
    pub rdepend: Vec<portage_atom::DepEntry>,
    /// BDEPEND — build-host dependencies (EAPI 7+).
    pub bdepend: Vec<portage_atom::DepEntry>,
    /// PDEPEND — post-merge dependencies.
    pub pdepend: Vec<portage_atom::DepEntry>,
    /// IDEPEND — install-time dependencies (EAPI 8+).
    pub idepend: Vec<portage_atom::DepEntry>,
}

/// Trait for a package repository that the solver can query.
///
/// Implementations provide package metadata sourced from ebuild caches,
/// as described in [PMS 7](https://projects.gentoo.org/pms/9/pms.html#mandatory-ebuilddefined-variables).
pub trait PackageRepository {
    /// Return all packages in the repository.
    fn all_packages(&self) -> Vec<Cpn>;

    /// Return all versions for a given CPN, with their metadata.
    fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, PackageVersions)>;

    /// The resolved **desired** USE state for a specific version.
    ///
    /// This is the caller's policy fully resolved — global USE (profile +
    /// `make.conf`), `package.use` overrides, and the ebuild's IUSE defaults all
    /// folded into one config.  The solver consumes this; it never resolves
    /// policy itself.  See `docs/use-and-solver-boundary.md`.
    fn desired_use(&self, cpv: &Cpv) -> UseConfig;
}

/// A simple in-memory repository for testing.
#[derive(Clone)]
pub struct InMemoryRepository {
    packages: HashMap<Cpn, Vec<(Cpv, PackageVersions)>>,
    /// Global desired USE, used by `desired_use` (folded with each version's
    /// IUSE defaults).  Tests set this instead of passing a config to the
    /// provider's constructor.
    use_config: UseConfig,
}

impl Default for InMemoryRepository {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryRepository {
    pub fn new() -> Self {
        Self {
            packages: HashMap::new(),
            use_config: UseConfig::new(),
        }
    }

    /// Set the global desired USE returned (folded with IUSE defaults) by
    /// [`PackageRepository::desired_use`].
    pub fn set_use_config(&mut self, config: UseConfig) {
        self.use_config = config;
    }

    pub fn add_version(
        &mut self,
        cpv: Cpv,
        slot: Option<Interned<DefaultInterner>>,
        subslot: Option<Interned<DefaultInterner>>,
        deps: PackageDeps,
    ) {
        self.add_version_full(cpv, slot, subslot, None, vec![], deps);
    }

    pub fn add_version_with_iuse(
        &mut self,
        cpv: Cpv,
        slot: Option<Interned<DefaultInterner>>,
        subslot: Option<Interned<DefaultInterner>>,
        iuse: Vec<Interned<DefaultInterner>>,
        deps: PackageDeps,
    ) {
        self.add_version_full(cpv, slot, subslot, None, iuse, deps);
    }

    /// Insert a fully-constructed [`PackageVersions`] for the given CPV.
    ///
    /// Use this when you already have slot, subslot, repo, iuse, iuse_defaults,
    /// and deps assembled — e.g. when bridging from an external metadata cache.
    pub fn add_package_versions(&mut self, cpv: Cpv, versions: PackageVersions) {
        let cpn = cpv.cpn;
        self.packages.entry(cpn).or_default().push((cpv, versions));
    }

    pub fn add_version_full(
        &mut self,
        cpv: Cpv,
        slot: Option<Interned<DefaultInterner>>,
        subslot: Option<Interned<DefaultInterner>>,
        repo: Option<Interned<DefaultInterner>>,
        iuse: Vec<Interned<DefaultInterner>>,
        deps: PackageDeps,
    ) {
        let cpn = cpv.cpn;
        self.packages.entry(cpn).or_default().push((
            cpv,
            PackageVersions {
                slot,
                subslot,
                repo,
                iuse,
                iuse_defaults: HashMap::new(),
                deps,
            },
        ));
    }
}

impl PackageRepository for InMemoryRepository {
    fn all_packages(&self) -> Vec<Cpn> {
        self.packages.keys().cloned().collect()
    }

    fn desired_use(&self, cpv: &Cpv) -> UseConfig {
        let mut cfg = self.use_config.clone();
        if let Some(versions) = self.packages.get(&cpv.cpn)
            && let Some((_, meta)) = versions.iter().find(|(c, _)| c.version == cpv.version)
        {
            cfg.fold_iuse_defaults(&meta.iuse_defaults);
        }
        cfg
    }

    fn versions_for(&self, cpn: &Cpn) -> Vec<(Cpv, PackageVersions)> {
        self.packages
            .get(cpn)
            .map(|v| {
                v.iter()
                    .map(|(cpv, meta)| {
                        (
                            cpv.clone(),
                            PackageVersions {
                                slot: meta.slot,
                                subslot: meta.subslot,
                                repo: meta.repo,
                                iuse: meta.iuse.clone(),
                                iuse_defaults: meta.iuse_defaults.clone(),
                                deps: PackageDeps {
                                    depend: meta.deps.depend.clone(),
                                    rdepend: meta.deps.rdepend.clone(),
                                    bdepend: meta.deps.bdepend.clone(),
                                    pdepend: meta.deps.pdepend.clone(),
                                    idepend: meta.deps.idepend.clone(),
                                },
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_repo() {
        let mut repo = InMemoryRepository::new();
        let cpv = Cpv::parse("dev-libs/openssl-3.0.0").unwrap();
        let cpn = Cpn::parse("dev-libs/openssl").unwrap();
        repo.add_version(
            cpv,
            Some(Interned::intern("0")),
            None,
            PackageDeps {
                depend: vec![],
                rdepend: vec![],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );
        assert_eq!(repo.all_packages().len(), 1);
        assert_eq!(repo.versions_for(&cpn).len(), 1);
    }
}
