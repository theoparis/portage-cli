use std::collections::{HashMap, HashSet};

use gentoo_core::Arch;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Operator, Version};
use portage_atom_pubgrub::{
    DroppedDep, IUseDefault, PackageDeps, PackageRepository, PackageVersions, RequiredUse,
};
use portage_metadata::{CacheEntry, Keyword, LicenseExpr, RequiredUseExpr, Stability};
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
pub(super) fn keyword_accepts(
    keywords: &[Keyword],
    arch: &str,
    accept_keywords: &[String],
) -> bool {
    if accept_keywords.iter().any(|k| k == "**") {
        return true;
    }
    if accept_keywords.is_empty() {
        // No ACCEPT_KEYWORDS loaded; the profile baseline is stable-only.
        return keywords
            .iter()
            .any(|kw| kw.arch.as_str() == arch && kw.stability == Stability::Stable);
    }
    // Portage semantics: accepting `~arch` (testing) implies accepting stable
    // `arch` too — testing is a superset of stable.  `*` accepts any stable
    // keyword for the arch, `~*` any testing keyword (which also implies stable).
    let testing_tok = format!("~{arch}");
    let accept_testing = accept_keywords
        .iter()
        .any(|k| *k == testing_tok || k == "~*");
    let accept_stable = accept_testing
        || accept_keywords
            .iter()
            .any(|k| k.as_str() == arch || k == "*");

    keywords.iter().any(|kw| {
        if kw.arch.as_str() != arch {
            return false;
        }
        match kw.stability {
            Stability::Stable => accept_stable,
            Stability::Testing => accept_testing,
            _ => false,
        }
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
            if accept.iter().any(|a| a == name) {
                vec![]
            } else {
                vec![name.clone()]
            }
        }
        LicenseExpr::AnyOf(children) => {
            if children.iter().any(|c| license_accepted(c, accept)) {
                vec![]
            } else {
                children
                    .first()
                    .map(|c| licenses_needed(c, accept))
                    .unwrap_or_default()
            }
        }
        LicenseExpr::All(children) => children
            .iter()
            .flat_map(|c| licenses_needed(c, accept))
            .collect(),
        LicenseExpr::UseConditional { entries, .. } => entries
            .iter()
            .flat_map(|e| licenses_needed(e, accept))
            .collect(),
    }
}

/// Check whether `mask_dep` matches the given `cpv` (version + CPN, no slot check).
/// Whether `cpv` (in `slot`) is masked: some mask atom matches and no unmask
/// atom does (`/etc/portage/package.unmask` cancels masks per package,
/// portage(5)).
pub(super) fn is_masked(
    masks: &[Dep],
    unmasks: &[Dep],
    cpv: &Cpv,
    slot: &portage_atom::Slot,
) -> bool {
    let hit = |m: &Dep| mask_matches(m, cpv) && mask_slot_matches(m, slot);
    masks.iter().any(hit) && !unmasks.iter().any(hit)
}

/// Whether a mask atom's `:slot[/subslot]` component (if any) matches the
/// candidate's slot. A versionless slot-scoped mask like
/// `dev-qt/qttranslations:5` must not mask other slots.
fn mask_slot_matches(mask_dep: &Dep, slot: &portage_atom::Slot) -> bool {
    match &mask_dep.slot_dep {
        Some(portage_atom::SlotDep::Slot { slot: Some(s), .. }) => {
            s.slot == slot.slot
                && match s.subslot {
                    Some(ms) => slot.subslot.is_some_and(|cs| cs == ms),
                    None => true,
                }
        }
        _ => true,
    }
}

pub(super) fn mask_matches(mask_dep: &Dep, cpv: &Cpv) -> bool {
    if mask_dep.cpn != cpv.cpn {
        return false;
    }
    let (Some(op), Some(mask_ver)) = (mask_dep.op, &mask_dep.version) else {
        return mask_dep.version.is_none();
    };
    let cand = &cpv.version;
    match op {
        Operator::Equal => {
            if mask_dep.glob {
                cand.glob_matches(mask_ver)
            } else {
                cand == mask_ver
            }
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
    /// The main repo's name; versions from overlays are recorded in `repo_of`.
    pub(super) repo_name: String,
    /// Source repo of overlay-provided versions (absent ⇒ the main repo).
    pub(super) repo_of: HashMap<Cpv, String>,
}

/// The repo a version comes from (for `::repo` display and constraints).
pub(super) fn repo_name_of<'a>(data: &'a RepoData, cpv: &Cpv) -> &'a str {
    data.repo_of
        .get(cpv)
        .map_or(data.repo_name.as_str(), String::as_str)
}

pub(super) struct Adapter<'a> {
    pub(super) data: &'a RepoData,
    pub(super) arch: &'a Arch,
    pub(super) accept_keywords: &'a [String],
    pub(super) package_mask: &'a [Dep],
    pub(super) package_unmask: &'a [Dep],
    pub(super) accept_license: &'a [String],
    /// Global desired USE (profile + make.conf), folded with per-version
    /// `package.use` + IUSE defaults by `desired_use`.
    pub(super) use_config: &'a portage_atom_pubgrub::UseConfig,
    pub(super) package_use: &'a [(Dep, Vec<String>)],
    /// Profile USE force/mask policy: applied to each version's effective USE and
    /// consulted by the Level-C cede gate (pinned flags are never ceded).
    pub(super) force_mask: &'a super::force_mask::ForceMask,
    /// Exact installed cpvs. A version that is installed and staying installed
    /// never has its `REQUIRED_USE` flags ceded — its USE was decided at build
    /// time, and only packages being built get theirs auto-satisfied (emerge
    /// likewise leaves installed packages' constraints alone).
    pub(super) installed_cpvs: &'a std::collections::HashSet<Cpv>,
    /// Level-C: when set, cede each package's non-pinned `REQUIRED_USE` flags to
    /// the solver (`SolverDecided`) instead of fixing them. See
    /// `portage-atom-pubgrub/docs/required-use-level-c.md`.
    pub(super) autosolve_use: bool,
}

/// Apply the ebuild's IUSE defaults: every IUSE flag not already set by the
/// resolved config takes its `+`/`-` default.
fn apply_iuse_defaults(
    cfg: &mut portage_atom_pubgrub::UseConfig,
    m: &portage_metadata::EbuildMetadata,
) {
    use portage_atom_pubgrub::UseFlagState;
    for iu in &m.iuse {
        let flag = Interned::intern(iu.name());
        if cfg.get_opt(&flag).is_none()
            && let Some(def) = iu.default
        {
            cfg.set(
                flag,
                match def {
                    portage_metadata::IUseDefault::Enabled => UseFlagState::Enabled,
                    portage_metadata::IUseDefault::Disabled => UseFlagState::Disabled,
                },
            );
        }
    }
}

impl Adapter<'_> {
    /// Level-C cede: when `--autosolve-use` is on and the package's REQUIRED_USE
    /// is *violated* by the resolved config, hand its REQUIRED_USE flags to the
    /// solver as preferences (`solver_decide`) — a ceded flag keeps its resolved
    /// value as the preference (greedy keep-config) and the solver only flips it
    /// to satisfy REQUIRED_USE. Flags the user pinned via package.use, or the
    /// profile forced/masked, are left fixed (hard choices we must not override).
    ///
    /// Skips ceding entirely when the constraint already holds, so settled flags
    /// (e.g. a USE_EXPAND like LLVM_SLOT/PYTHON_TARGETS) are not re-decided and
    /// their conditional deps not gratuitously pulled — matching emerge, which
    /// acts only on violations.
    fn cede_required_use(
        &self,
        cfg: &mut portage_atom_pubgrub::UseConfig,
        m: &portage_metadata::EbuildMetadata,
        cpv: &Cpv,
        slot: Option<Interned<DefaultInterner>>,
        stable: bool,
    ) {
        use portage_atom_pubgrub::{UseConfig, UseFlagState, apply_package_use};

        if self.installed_cpvs.contains(cpv) {
            return;
        }
        let Some(ru) = &m.required_use else {
            return;
        };
        let enabled =
            |flag: &str| matches!(cfg.get(&Interned::intern(flag)), UseFlagState::Enabled);
        if ru.unsatisfied(&enabled).is_empty() {
            return;
        }

        // Flags the user pinned via package.use: applying it to an empty base
        // leaves exactly those flags set.
        let empty = UseConfig::new();
        let pins = apply_package_use(&empty, cpv, slot, self.package_use);
        let iuse: std::collections::HashSet<&str> = m.iuse.iter().map(|iu| iu.name()).collect();
        // Flags pinned by use.force/use.mask (global, package-level and the stable
        // variants): hard profile decisions, never ceded.
        let forced_masked = self.force_mask.pins(cpv, stable);
        let mut names = std::collections::BTreeSet::new();
        collect_required_use_flags(ru, &mut names);
        for name in names {
            let flag = Interned::intern(&name);
            // Only cede real flags the user has not pinned or the profile has not
            // forced/masked.
            if !iuse.contains(name.as_str())
                || pins.get_opt(&flag).is_some()
                || forced_masked.contains(name.as_str())
            {
                continue;
            }
            let prefer = matches!(cfg.get(&flag), UseFlagState::Enabled);
            cfg.solver_decide(flag, prefer);
        }
    }
}

impl PackageRepository for Adapter<'_> {
    fn all_packages(&self) -> Vec<Cpn> {
        self.data.cpns.clone()
    }

    fn desired_use(&self, cpv: &Cpv) -> portage_atom_pubgrub::UseConfig {
        use portage_atom_pubgrub::{UseConfig, apply_package_use};

        let meta = self
            .data
            .versions
            .get(&cpv.cpn)
            .and_then(|entries| entries.iter().find(|(c, _)| c.version == cpv.version))
            .map(|(_, cache)| &cache.metadata);

        let slot = meta.and_then(|m| {
            let s = m.slot.slot;
            if s.as_str().is_empty() { None } else { Some(s) }
        });

        // Caller-resolved policy: package.use over global USE, then the ebuild's
        // IUSE defaults for anything still unset → the authoritative desired set.
        let mut cfg: UseConfig =
            apply_package_use(self.use_config, cpv, slot, self.package_use).into_owned();
        if let Some(m) = meta {
            apply_iuse_defaults(&mut cfg, m);
        }

        // Profile USE force/mask override package.use and the configured value
        // (Portage semantics). Global use.force/use.mask are already in the base
        // config; this layers the package-level sets plus the *.stable.* sets,
        // the latter only when this version is merged due to a stable keyword.
        // This is what makes crossdev's package.use.force/mask (multilib/cet/…)
        // take effect on cross-* packages.
        let stable = meta.is_some_and(|m| {
            super::force_mask::is_stable(&m.keywords, self.arch.as_str(), self.accept_keywords)
        });
        if !self.force_mask.is_empty() {
            self.force_mask.apply(&mut cfg, cpv, stable);
        }

        // Level-C: cede this package's REQUIRED_USE flags to the solver.
        if self.autosolve_use
            && let Some(m) = meta
        {
            self.cede_required_use(&mut cfg, m, cpv, slot, stable);
        }
        cfg
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
                        if !keyword_accepts(
                            &meta.keywords,
                            self.arch.as_str(),
                            self.accept_keywords,
                        ) {
                            return false;
                        }
                        // Mask check
                        if is_masked(
                            self.package_mask,
                            self.package_unmask,
                            cpv,
                            &cache.metadata.slot,
                        ) {
                            return false;
                        }
                        // License check
                        if let Some(lic) = &meta.license
                            && !license_accepted(lic, self.accept_license)
                        {
                            return false;
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
                        let repo = Some(Interned::<DefaultInterner>::intern(repo_name_of(
                            self.data, cpv,
                        )));
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
                        // Translate the parsed metadata grammar into the solver's
                        // interned-flag fact vocabulary (the crate stays free of
                        // portage-metadata). Dormant until Level-C consumes it.
                        let required_use = meta.required_use.as_ref().map(translate_required_use);
                        (
                            cpv.clone(),
                            PackageVersions {
                                slot,
                                subslot,
                                repo,
                                iuse,
                                iuse_defaults,
                                deps,
                                required_use,
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Translate `portage_metadata::RequiredUseExpr` (string flags) into the
/// solver's `RequiredUse` fact (interned flags).
///
/// This is the caller's adaptation step — it keeps `portage-atom-pubgrub`
/// decoupled from the md5-cache parser, mirroring how dep strings become
/// `DepEntry`. Pure structural translation; no policy.
fn translate_required_use(expr: &RequiredUseExpr) -> RequiredUse {
    let kids = |v: &[RequiredUseExpr]| v.iter().map(translate_required_use).collect();
    match expr {
        RequiredUseExpr::Flag { name, negated } => RequiredUse::Flag {
            name: Interned::intern(name),
            negated: *negated,
        },
        RequiredUseExpr::AnyOf(c) => RequiredUse::AnyOf(kids(c)),
        RequiredUseExpr::ExactlyOne(c) => RequiredUse::ExactlyOne(kids(c)),
        RequiredUseExpr::AtMostOne(c) => RequiredUse::AtMostOne(kids(c)),
        RequiredUseExpr::UseConditional {
            flag,
            negated,
            entries,
        } => RequiredUse::UseConditional {
            flag: Interned::intern(flag),
            negated: *negated,
            entries: kids(entries),
        },
        RequiredUseExpr::All(c) => RequiredUse::All(kids(c)),
    }
}

/// Collect every flag name mentioned in a `REQUIRED_USE` expression (guards and
/// operands, ignoring `!`), for deciding which flags to cede.
fn collect_required_use_flags(
    expr: &RequiredUseExpr,
    out: &mut std::collections::BTreeSet<String>,
) {
    match expr {
        RequiredUseExpr::Flag { name, .. } => {
            out.insert(name.clone());
        }
        RequiredUseExpr::AnyOf(c)
        | RequiredUseExpr::ExactlyOne(c)
        | RequiredUseExpr::AtMostOne(c)
        | RequiredUseExpr::All(c) => {
            for e in c {
                collect_required_use_flags(e, out);
            }
        }
        RequiredUseExpr::UseConditional { flag, entries, .. } => {
            out.insert(flag.clone());
            for e in entries {
                collect_required_use_flags(e, out);
            }
        }
    }
}

/// Load the main repo's md5-cache plus every overlay's metadata (sourcing
/// cache-less ebuilds — see `overlay::overlay_entries`). A cpv provided by
/// the main repo wins over an overlay copy; among overlays, earlier wins.
pub(super) async fn load_repos(
    repo: &Repository,
    overlays: &[(Repository, Vec<Repository>)],
) -> RepoData {
    let mut cpns_set: HashSet<Cpn> = HashSet::new();
    let mut versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
    let mut repo_of: HashMap<Cpv, String> = HashMap::new();
    let mut seen: HashSet<Cpv> = HashSet::new();

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
            seen.insert(cpv.clone());
            versions.entry(cpn).or_default().push((cpv, entry));
        }
    }

    for (overlay, masters) in overlays {
        for (cpv, entry) in super::overlay::overlay_entries(overlay, masters).await {
            if !seen.insert(cpv.clone()) {
                continue;
            }
            cpns_set.insert(cpv.cpn);
            repo_of.insert(cpv.clone(), overlay.name().to_string());
            versions.entry(cpv.cpn).or_default().push((cpv, entry));
        }
    }

    let mut cpns: Vec<Cpn> = cpns_set.into_iter().collect();
    cpns.sort_by_key(|c| format!("{}/{}", c.category, c.package));

    RepoData {
        cpns,
        versions,
        repo_name: repo.name().to_string(),
        repo_of,
    }
}

/// Map a dep atom to a `PortagePackage` for the solver.
pub(super) fn target_package(
    data: &RepoData,
    dep: &Dep,
    arch: &Arch,
    accept_keywords: &[String],
    package_mask: &[Dep],
    package_unmask: &[Dep],
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
                && !is_masked(package_mask, package_unmask, cpv, &cache.metadata.slot)
                && cache
                    .metadata
                    .license
                    .as_ref()
                    .is_none_or(|l| license_accepted(l, accept_license))
        })
        .collect();

    if arch_entries.is_empty() {
        return portage_atom_pubgrub::PortagePackage::unslotted(dep.cpn);
    }

    let mut slots: Vec<_> = arch_entries
        .iter()
        .filter_map(|(_, cache)| {
            let s = &cache.metadata.slot.slot;
            if s.as_str().is_empty() {
                None
            } else {
                Some(*s)
            }
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
                    if s.as_str().is_empty() {
                        None
                    } else {
                        Some((cpv.version.clone(), *s))
                    }
                })
                .max_by(|a, b| a.0.cmp(&b.0))
                .map(|(_, s)| s)
                .unwrap();
            portage_atom_pubgrub::PortagePackage::slotted(dep.cpn, best)
        }
    }
}

/// Collect all dependency CPNs referenced in a package version's raw dep data.
///
/// Walks the full dep tree (across all dep classes, through all conditional and
/// group nodes) so that masked transitive deps are captured regardless of USE
/// flag state at detection time.
pub(super) fn cpns_for(data: &RepoData, cpn: &Cpn, ver: &Version) -> Vec<Cpn> {
    use portage_atom::DepEntry;

    fn walk(entries: &[DepEntry], out: &mut Vec<Cpn>) {
        for e in entries {
            match e {
                DepEntry::Atom(dep) if dep.blocker.is_none() => out.push(dep.cpn),
                DepEntry::UseConditional { children, .. }
                | DepEntry::AnyOf(children)
                | DepEntry::AllOf(children)
                | DepEntry::ExactlyOneOf(children)
                | DepEntry::AtMostOneOf(children) => walk(children, out),
                _ => {}
            }
        }
    }

    let Some(entries) = data.versions.get(cpn) else {
        return vec![];
    };
    let Some((_, cache)) = entries.iter().find(|(cpv, _)| &cpv.version == ver) else {
        return vec![];
    };
    let meta = &cache.metadata;
    let mut out = Vec::new();
    for deps in [
        &meta.depend,
        &meta.rdepend,
        &meta.bdepend,
        &meta.pdepend,
        &meta.idepend,
    ] {
        walk(deps, &mut out);
    }
    out
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
    package_unmask: &[Dep],
    accept_license: &[String],
) -> Vec<AutounmaskCandidate> {
    let mut candidates = Vec::new();

    for dep in dropped {
        if dep.package.is_virtual() {
            continue;
        }
        // || group with available alternatives: the solver picked one of them,
        // no need to unmask/keyword the dropped branch.
        if !dep.alternatives.is_empty() {
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
            if is_masked(package_mask, package_unmask, cpv, &meta.slot) {
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

    // A CPV may appear from multiple DroppedDep entries; its reasons are
    // determined solely by its own metadata, so we keep the first occurrence.
    let mut seen: HashSet<String> = HashSet::new();
    candidates.retain(|c| seen.insert(c.cpv.to_string()));

    candidates
}

#[cfg(test)]
mod tests {
    use super::super::force_mask::{ForceMask, index_by_cpn};
    use super::*;
    use portage_atom_pubgrub::{PackageRepository, UseConfig, UseFlagState};

    fn dep(s: &str) -> Dep {
        Dep::parse(s).unwrap()
    }

    /// Build a one-package `RepoData` from md5-cache text.
    fn repo_with(cpv: &str, cache_text: &str) -> (RepoData, Cpv) {
        let cpv = Cpv::parse(cpv).unwrap();
        let entry = CacheEntry::parse(cache_text).unwrap();
        let mut versions = HashMap::new();
        versions.insert(cpv.cpn, vec![(cpv.clone(), entry)]);
        (
            RepoData {
                repo_of: Default::default(),
                cpns: vec![cpv.cpn],
                versions,
                repo_name: "test".into(),
            },
            cpv,
        )
    }

    // A flag forced by use.force must not be ceded to the solver even when it is
    // named in a violated REQUIRED_USE — only the non-forced flag is ceded, so
    // the solver can never produce a plan that flips a forced flag.
    #[test]
    fn forced_flag_is_not_ceded() {
        let (data, cpv) = repo_with(
            "cat/pkg-1.0",
            "EAPI=7\nSLOT=0\nIUSE=a b\nREQUIRED_USE=?? ( a b )\nKEYWORDS=amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        let mut use_config = UseConfig::new();
        use_config.enable(Interned::intern("a"));
        use_config.enable(Interned::intern("b")); // both on ⇒ ?? ( a b ) violated

        let fm = ForceMask {
            use_force: vec!["a".to_string()],
            ..Default::default()
        };
        let adapter = Adapter {
            data: &data,
            arch: &arch,
            accept_keywords: &["amd64".to_string()],
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            accept_license: &["*".to_string()],
            use_config: &use_config,
            package_use: &[],
            force_mask: &fm, // a is use.force'd
            autosolve_use: true,
        };

        let desired = adapter.desired_use(&cpv);
        assert!(
            matches!(desired.get(&Interned::intern("a")), UseFlagState::Enabled),
            "forced flag a must stay fixed-enabled, not ceded"
        );
        assert!(
            matches!(
                desired.get(&Interned::intern("b")),
                UseFlagState::SolverDecided { .. }
            ),
            "non-forced flag b should be ceded to satisfy the violated ?? ( a b )"
        );
    }

    // Without the force pin, both flags in the violated constraint are ceded.
    #[test]
    fn unforced_flags_are_ceded() {
        let (data, cpv) = repo_with(
            "cat/pkg-1.0",
            "EAPI=7\nSLOT=0\nIUSE=a b\nREQUIRED_USE=?? ( a b )\nKEYWORDS=amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        let mut use_config = UseConfig::new();
        use_config.enable(Interned::intern("a"));
        use_config.enable(Interned::intern("b"));

        let fm = ForceMask::default();
        let adapter = Adapter {
            data: &data,
            arch: &arch,
            accept_keywords: &["amd64".to_string()],
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            accept_license: &["*".to_string()],
            use_config: &use_config,
            package_use: &[],
            force_mask: &fm,
            autosolve_use: true,
        };

        let desired = adapter.desired_use(&cpv);
        for f in ["a", "b"] {
            assert!(
                matches!(
                    desired.get(&Interned::intern(f)),
                    UseFlagState::SolverDecided { .. }
                ),
                "flag {f} should be ceded when nothing pins it"
            );
        }
    }

    // A satisfied REQUIRED_USE cedes nothing (cede gating).
    #[test]
    fn satisfied_constraint_cedes_nothing() {
        let (data, cpv) = repo_with(
            "cat/pkg-1.0",
            "EAPI=7\nSLOT=0\nIUSE=a b\nREQUIRED_USE=?? ( a b )\nKEYWORDS=amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        let mut use_config = UseConfig::new();
        use_config.enable(Interned::intern("a")); // only a on ⇒ ?? satisfied

        let fm = ForceMask::default();
        let adapter = Adapter {
            data: &data,
            arch: &arch,
            accept_keywords: &["amd64".to_string()],
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            accept_license: &["*".to_string()],
            use_config: &use_config,
            package_use: &[],
            force_mask: &fm,
            autosolve_use: true,
        };

        let desired = adapter.desired_use(&cpv);
        for f in ["a", "b"] {
            assert!(
                !matches!(
                    desired.get(&Interned::intern(f)),
                    UseFlagState::SolverDecided { .. }
                ),
                "flag {f} must not be ceded when REQUIRED_USE already holds"
            );
        }
    }

    // package.use.force/mask now change a package's effective USE (not just the
    // cede gate). This is the crossdev case: force multilib on, mask cet off.
    #[test]
    fn package_force_mask_change_effective_use() {
        let (data, cpv) = repo_with(
            "cross-foo/gcc-13.2",
            "EAPI=8\nSLOT=0\nIUSE=multilib cet\nKEYWORDS=~amd64\nDESCRIPTION=t\n",
        );
        let arch = Arch::intern("amd64");
        let mut use_config = UseConfig::new();
        use_config.enable(Interned::intern("cet")); // user enabled a flag the profile masks

        let fm = ForceMask {
            pkg_force: index_by_cpn(vec![(dep("cross-foo/gcc"), vec!["multilib".to_string()])]),
            pkg_mask: index_by_cpn(vec![(dep("cross-foo/gcc"), vec!["cet".to_string()])]),
            ..Default::default()
        };
        let adapter = Adapter {
            data: &data,
            arch: &arch,
            accept_keywords: &["~amd64".to_string()],
            package_mask: &[],
            package_unmask: &[],
            installed_cpvs: &std::collections::HashSet::new(),
            accept_license: &["*".to_string()],
            use_config: &use_config,
            package_use: &[],
            force_mask: &fm,
            autosolve_use: false,
        };

        let desired = adapter.desired_use(&cpv);
        assert_eq!(
            desired.get(&Interned::intern("multilib")),
            UseFlagState::Enabled,
            "package.use.force must enable multilib"
        );
        assert_eq!(
            desired.get(&Interned::intern("cet")),
            UseFlagState::Disabled,
            "package.use.mask must beat the user's enable of cet"
        );
    }
}
