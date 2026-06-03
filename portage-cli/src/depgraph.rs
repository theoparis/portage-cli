use std::collections::HashMap;

use camino::Utf8Path;
use gentoo_core::Arch;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Operator, Version};
use portage_atom_pubgrub::{
    DepClass, IUseDefault, InstalledPackage as SolverInstalledPackage, InstalledPolicy,
    PackageDeps, PackageRepository, PackageVersions, PortageDependencyProvider, PortagePackage,
    PortageVersionSet, UseConfig, UseFlagRequirement, UseFlagState,
};
use portage_metadata::{CacheEntry, Keyword, Stability};
use portage_repo::{CacheReadOpts, ProfileStack, Repository, DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF, cache_entries_parallel};
use portage_vdb::Vdb;

use crate::cli::DepgraphFormat;

// ---------------------------------------------------------------------------
// Repository adapter
// ---------------------------------------------------------------------------

fn keyword_accepts(keywords: &[Keyword], arch: &str) -> bool {
    keywords.iter().any(|kw| {
        kw.arch.as_str() == arch && matches!(kw.stability, Stability::Stable | Stability::Testing)
    })
}

struct RepoData {
    cpns: Vec<Cpn>,
    versions: HashMap<Cpn, Vec<(Cpv, CacheEntry)>>,
    repo_name: String,
}

struct Adapter<'a> {
    data: &'a RepoData,
    arch: &'a Arch,
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

async fn load_repo(repo: &Repository) -> RepoData {
    use std::collections::HashSet;
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

/// Map a user-supplied dep atom to the `PortagePackage` the solver should
/// resolve it against.
///
/// Only versions with acceptable keywords for `arch` are considered when
/// selecting the slot, so we never ask the solver for a slot that has no
/// resolvable versions.
fn target_package(data: &RepoData, dep: &Dep, arch: &Arch) -> PortagePackage {
    let entries = match data.versions.get(&dep.cpn) {
        Some(e) => e,
        None => return PortagePackage::unslotted(dep.cpn),
    };

    // Only consider versions that have acceptable keywords for this arch.
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
            // Multiple arch-compatible slots: pick the one containing the
            // highest version.
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

/// Look up the cache entry for a resolved `(PortagePackage, Version)` pair.
fn find_cache<'a>(
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

// ---------------------------------------------------------------------------
// VDB (installed packages)
// ---------------------------------------------------------------------------

/// A record of one installed package from the VDB.
struct VdbEntry {
    cpn: Cpn,
    slot: Option<String>,
    version: Version,
    active_use: Vec<Interned<DefaultInterner>>,
    iuse: Vec<Interned<DefaultInterner>>,
}

/// Load all installed packages from the VDB.
///
/// Returns an empty vec if the VDB is unavailable (non-Gentoo system, etc.).
fn load_installed() -> Vec<VdbEntry> {
    let Ok(vdb) = Vdb::open_default() else {
        return Vec::new();
    };
    vdb.packages()
        .into_iter()
        .map(|pkg| {
            let active_use = pkg
                .use_flags()
                .unwrap_or_default()
                .into_iter()
                .map(|f| Interned::intern(&f))
                .collect();
            // Strip +/- prefix from IUSE entries (e.g. "+net" → "net").
            let iuse = pkg
                .iuse()
                .unwrap_or_default()
                .into_iter()
                .map(|f| Interned::intern(f.trim_start_matches(['+', '-'])))
                .collect();
            VdbEntry {
                cpn: *pkg.cpn(),
                slot: pkg.slot_main().ok(),
                version: pkg.cpv().version.clone(),
                active_use,
                iuse,
            }
        })
        .collect()
}


/// Determine the emerge-style action tag for a package.
///
/// Compares `ver` against the installed version for the **same slot**.
/// Cross-slot installs (e.g. installing rust-bin:1.89 when only :1.93 is
/// present) are always `N` — they are new slot installations, not downgrades.
///
/// - `N` — this slot is not installed
/// - `U` — upgrade within this slot
/// - `D` — downgrade within this slot (rare but possible)
fn action_tag(pkg: &PortagePackage, ver: &Version, installed: &HashMap<Cpn, HashMap<String, Version>>) -> &'static str {
    let Some(by_slot) = installed.get(pkg.cpn()) else {
        return "N";
    };
    let slot_key = pkg.slot()
        .map(|s| s.as_str().to_string())
        .unwrap_or_default();
    match by_slot.get(&slot_key) {
        None => "N",
        Some(inst) => {
            if ver > inst { "U" } else if ver < inst { "D" } else { "R" }
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn depgraph(
    repo_path: &Utf8Path,
    atoms: &[String],
    arch: &Arch,
    format: DepgraphFormat,
) -> crate::error::Result<()> {
    // Open once for the async path (build_use_config needs a shell).
    let repo = Repository::open(repo_path)
        .map_err(|e| crate::error::Error::Other(e.to_string()))?;

    let (data, installed_entries, (use_config, use_expand)) = tokio::join!(
        load_repo(&repo),
        async { load_installed() },
        build_use_config(&repo),
    );

    // Exact-CPV set: O(1) lookup for filtering packages already at the right version.
    let installed_cpvs: std::collections::HashSet<Cpv> = installed_entries
        .iter()
        .map(|e| Cpv::new(e.cpn, e.version.clone()))
        .collect();

    // CPN → (slot → installed version): slot-aware action tags (N/U/D/R).
    let mut installed: HashMap<Cpn, HashMap<String, Version>> = HashMap::new();
    for e in &installed_entries {
        let slot_key = e.slot.clone().unwrap_or_default();
        installed.entry(e.cpn).or_default().insert(slot_key, e.version.clone());
    }

    let adapter = Adapter { data: &data, arch };
    let mut provider = PortageDependencyProvider::new(adapter, use_config.clone(), &[]);

    // Register every installed package with the solver so it prefers the
    // already-installed version when the constraint is satisfied.  This makes
    // the solver behave like portage: don't propose upgrades when unnecessary.
    for e in &installed_entries {
        let pkg = match e.slot.as_deref().filter(|s| !s.is_empty()) {
            Some(s) => PortagePackage::slotted(e.cpn, Interned::intern(s)),
            None => PortagePackage::unslotted(e.cpn),
        };
        provider.add_installed(SolverInstalledPackage {
            package: pkg,
            version: e.version.clone(),
            policy: InstalledPolicy::Favor,
            active_use: e.active_use.clone(),
            iuse: e.iuse.clone(),
        });
    }

    let mut root_deps = Vec::new();
    for target in atoms {
        let dep = Dep::parse(target).map_err(|e| {
            crate::error::Error::Other(format!("bad atom '{}': {}", target, e))
        })?;
        let pkg = target_package(&data, &dep, arch);
        let vs = match &dep.version {
            Some(v) => {
                let op = dep.op.unwrap_or(Operator::GreaterOrEqual);
                PortageVersionSet::from_operator(op, dep.glob, v.clone())
            }
            None => PortageVersionSet::any(),
        };
        root_deps.push((pkg, vs));
    }

    report_dropped_deps(provider.dropped_deps(), &data, arch.as_str());

    let solution = provider
        .resolve_targets(root_deps)
        .map_err(|e| crate::error::Error::Other(format!("resolution failed: {:?}", e)))?;

    // Strip internal solver bookkeeping nodes, then filter out packages that
    // are already installed at exactly the version the solver selected.
    let mut order: Vec<_> = provider
        .install_order(&solution)
        .into_iter()
        .filter(|(pkg, ver)| {
            if pkg.is_virtual() {
                return false;
            }
            let cpv = Cpv::new(*pkg.cpn(), ver.clone());
            !installed_cpvs.contains(&cpv)
        })
        .collect();

    // Append reinstall packages: installed packages whose USE dep constraints
    // are violated by the resolved set.  They share the same CPV as the
    // installed version (so the exact-CPV filter above drops them), but need
    // a rebuild with different USE flags — portage shows these as action `R`.
    // Exclude any package already in the order (e.g. being upgraded to a new
    // version, which also satisfies the USE dep change implicitly).
    {
        let in_order: std::collections::HashSet<Cpn> =
            order.iter().map(|(pkg, _)| *pkg.cpn()).collect();
        // For reinstall entries with upgrade_to: emit the newer version (U).
        // For plain reinstalls: emit the installed version (R).
        let to_reinstall: Vec<(PortagePackage, Version)> = provider
            .reinstall_deps()
            .into_iter()
            .filter(|r| !in_order.contains(r.package.cpn()))
            .map(|r| {
                let ver = r.upgrade_to.as_ref().unwrap_or(&r.version).clone();
                (r.package.clone(), ver)
            })
            .collect();
        order.extend(to_reinstall);
    }
    let edges: Vec<_> = provider
        .dependency_graph(&solution)
        .into_iter()
        .filter(|e| !e.from.0.is_virtual() && !e.to.0.is_virtual())
        .collect();

    // Build a per-package lookup of required USE flag changes for output annotation.
    let flag_reqs: std::collections::HashMap<&PortagePackage, &UseFlagRequirement> = provider
        .use_flag_requirements()
        .iter()
        .map(|r| (&r.package, r))
        .collect();

    match format {
        DepgraphFormat::Pretty => {
            print_pretty(&data, &order, &edges, &installed, &use_config, &use_expand, &flag_reqs)
        }
        DepgraphFormat::Json => print_json(&data, &order, &edges, &installed, &use_expand, &flag_reqs),
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// USE config
// ---------------------------------------------------------------------------

/// Build a [`UseConfig`] by sourcing the full profile stack through a real
/// brush shell, then reading the resulting `USE` variable.
///
/// This mirrors exactly what portage does: each `make.defaults` is sourced in
/// order (so `USE="${USE} more"` expansions work correctly), `make.conf` is
/// sourced on top, then `use.force`/`use.mask` are applied, and `USE_EXPAND`
/// variables are expanded into flag tokens.
///
/// Falls back to an empty config on non-Gentoo environments.
/// Returns `(UseConfig, use_expand_keys)` where `use_expand_keys` is the list
/// of USE_EXPAND variable names (e.g. `["ABI_X86", "PERL_FEATURES", ...]`) for
/// grouping flags in the output display.
async fn build_use_config(repo: &Repository) -> (UseConfig, Vec<String>) {
    let (use_str, use_expand) = compute_use_env(repo).await
        .unwrap_or_default();
    let mut config = UseConfig::new();
    for token in use_str.split_whitespace() {
        if let Some(name) = token.strip_prefix('-') {
            config.set(Interned::intern(name), UseFlagState::Disabled);
        } else {
            config.set(Interned::intern(token), UseFlagState::Enabled);
        }
    }
    (config, use_expand)
}

async fn compute_use_env(repo: &Repository) -> Option<(String, Vec<String>)> {
    let profile_path = std::fs::canonicalize("/etc/portage/make.profile").ok()?;
    let stack = ProfileStack::build(profile_path).ok()?;
    let mut shell = repo.shell().await.ok()?;

    let make_conf: Option<std::path::PathBuf> = [DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF]
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(std::path::PathBuf::from);

    let confs: Vec<&std::path::Path> = make_conf.as_deref().into_iter().collect();
    stack.configure_shell(&mut shell, &confs).await.ok()?;

    let use_str = shell.get_var("USE").unwrap_or_default();
    let use_expand: Vec<String> = shell.get_var("USE_EXPAND")
        .unwrap_or_default()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    Some((use_str, use_expand))
}

// ---------------------------------------------------------------------------
// Dropped-dep reporting
// ---------------------------------------------------------------------------

/// Report dropped deps, distinguishing truly-missing packages from packages
/// that are in the repo but have no acceptable keyword for the target arch.
///
/// Arch-filtered drops are expected and produce only a summary count.
/// Truly-missing packages (not in the repo at all) are listed by name since
/// they may indicate a broken dep or a missing overlay.
fn report_dropped_deps(
    dropped: &[(PortagePackage, PortageVersionSet)],
    data: &RepoData,
    arch: &str,
) {
    let non_virtual: Vec<_> = dropped
        .iter()
        .filter(|(pkg, _)| !pkg.is_virtual())
        .collect();

    if non_virtual.is_empty() {
        return;
    }

    let mut truly_missing: std::collections::BTreeSet<String> = Default::default();
    let mut arch_filtered: std::collections::BTreeSet<String> = Default::default();

    for (pkg, _) in &non_virtual {
        let cpn_str = pkg.cpn().to_string();
        if data.versions.contains_key(pkg.cpn()) {
            arch_filtered.insert(cpn_str);
        } else {
            truly_missing.insert(cpn_str);
        }
    }

    if !truly_missing.is_empty() {
        eprintln!(
            "warning: {} package(s) not found in repo: {}",
            truly_missing.len(),
            truly_missing.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    if !arch_filtered.is_empty() {
        eprintln!(
            "note: {} package(s) skipped (no keywords for {}): {}",
            arch_filtered.len(),
            arch,
            arch_filtered.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
}

// ---------------------------------------------------------------------------
// Pretty output — emerge -p style
// ---------------------------------------------------------------------------

/// Format the USE flags and USE_EXPAND groups for pretty output.
///
/// Each flag token is annotated with a suffix when a `UseFlagRequirement` is
/// present for this package:
/// - `flag*`  — flag must be **enabled** but is not (needs to be added)
/// - `-flag%` — flag must be **disabled** but is enabled (needs to be removed)
///
/// Flags matching a USE_EXPAND key are grouped into their own `KEY="..."` section.
/// Returns an empty string when there are no flags.
fn format_flags(
    cache: &CacheEntry,
    use_config: &UseConfig,
    use_expand: &[String],
    req: Option<&UseFlagRequirement>,
) -> String {
    let mut base_flags: Vec<String> = Vec::new();
    // key → list of flag tokens
    let mut expand_groups: std::collections::BTreeMap<&str, Vec<String>> =
        std::collections::BTreeMap::new();

    let mut flags: Vec<_> = cache.metadata.iuse.iter().collect();
    flags.sort_by_key(|f| f.name());

    for f in flags {
        let name = f.name();
        let iuse_default_enabled =
            matches!(f.default, Some(portage_metadata::IUseDefault::Enabled));
        let interned = Interned::intern(name);
        let enabled = match use_config.get_opt(&interned) {
            Some(UseFlagState::Enabled) => true,
            Some(_) => false,
            None => iuse_default_enabled,
        };
        let sign = if enabled { "" } else { "-" };

        // Annotate flags that have a USE dep requirement attached.
        let suffix = if let Some(r) = req {
            if r.required_enabled.contains(&interned) {
                "*" // must be enabled but isn't
            } else if r.required_disabled.contains(&interned) {
                "%" // must be disabled but is
            } else {
                ""
            }
        } else {
            ""
        };

        let expand_match = use_expand.iter().find(|key| {
            let prefix = format!("{}_", key.to_lowercase());
            name.starts_with(prefix.as_str())
        });

        if let Some(key) = expand_match {
            let prefix = format!("{}_", key.to_lowercase());
            let short = &name[prefix.len()..];
            expand_groups
                .entry(key.as_str())
                .or_default()
                .push(format!("{sign}{short}{suffix}"));
        } else {
            base_flags.push(format!("{sign}{name}{suffix}"));
        }
    }

    let mut parts = Vec::new();
    if !base_flags.is_empty() {
        parts.push(format!("  USE=\"{}\"", base_flags.join(" ")));
    }
    for (key, tokens) in &expand_groups {
        parts.push(format!("  {}=\"{}\"", key, tokens.join(" ")));
    }
    parts.join("")
}

fn print_pretty(
    data: &RepoData,
    order: &[(PortagePackage, Version)],
    _edges: &[portage_atom_pubgrub::DepEdge],
    installed: &HashMap<Cpn, HashMap<String, Version>>,
    use_config: &UseConfig,
    use_expand: &[String],
    flag_reqs: &std::collections::HashMap<&PortagePackage, &UseFlagRequirement>,
) {
    println!("These are the packages that would be merged, in order:\n");
    println!("Calculating dependencies... done!");

    for (pkg, ver) in order {
        let cpn = pkg.cpn();
        let tag = action_tag(pkg, ver, installed);
        let repo = &data.repo_name;
        let req = flag_reqs.get(pkg).copied();

        let flag_str = find_cache(data, pkg, ver)
            .map(|c| format_flags(c, use_config, use_expand, req))
            .unwrap_or_default();

        println!("[ebuild  {tag:<6}] {cpn}-{ver}::{repo}{flag_str}");
    }

    println!("\nTotal: {} package(s)", order.len());
}

// ---------------------------------------------------------------------------
// JSON output
// ---------------------------------------------------------------------------

fn class_str(c: DepClass) -> &'static str {
    match c {
        DepClass::Depend => "DEPEND",
        DepClass::Rdepend => "RDEPEND",
        DepClass::Bdepend => "BDEPEND",
        DepClass::Pdepend => "PDEPEND",
        DepClass::Idepend => "IDEPEND",
    }
}

fn print_json(
    data: &RepoData,
    order: &[(PortagePackage, Version)],
    edges: &[portage_atom_pubgrub::DepEdge],
    installed: &HashMap<Cpn, HashMap<String, Version>>,
    _use_expand: &[String],
    flag_reqs: &std::collections::HashMap<&PortagePackage, &UseFlagRequirement>,
) {
    let packages: Vec<serde_json::Value> = order
        .iter()
        .map(|(pkg, ver)| {
            let cpn = pkg.cpn();
            let status = match action_tag(pkg, ver, installed) {
                "U" => "upgrade",
                "D" => "downgrade",
                "R" => "reinstall",
                _ => "new",
            };
            let mut obj = serde_json::json!({
                "atom": format!("{cpn}-{ver}"),
                "cpn": cpn.to_string(),
                "version": ver.to_string(),
                "repo": data.repo_name,
                "status": status,
            });
            if let Some(cache) = find_cache(data, pkg, ver) {
                let slot = &cache.metadata.slot;
                obj["slot"] = serde_json::Value::String(slot.slot.as_str().to_owned());
                if let Some(sub) = slot.subslot {
                    obj["subslot"] = serde_json::Value::String(sub.as_str().to_owned());
                }
                let iuse: Vec<String> = {
                    let mut flags: Vec<_> = cache.metadata.iuse.iter().collect();
                    flags.sort_by_key(|f| f.name());
                    flags.iter().map(|f| match f.default {
                        Some(portage_metadata::IUseDefault::Enabled) => {
                            format!("+{}", f.name())
                        }
                        _ => format!("-{}", f.name()),
                    }).collect()
                };
                obj["iuse"] = serde_json::json!(iuse);
            }
            if let Some(req) = flag_reqs.get(pkg) {
                if !req.required_enabled.is_empty() {
                    let flags: Vec<&str> =
                        req.required_enabled.iter().map(|f| f.as_str()).collect();
                    obj["required_use_enabled"] = serde_json::json!(flags);
                }
                if !req.required_disabled.is_empty() {
                    let flags: Vec<&str> =
                        req.required_disabled.iter().map(|f| f.as_str()).collect();
                    obj["required_use_disabled"] = serde_json::json!(flags);
                }
            }
            obj
        })
        .collect();

    let dep_edges: Vec<serde_json::Value> = edges
        .iter()
        .map(|e| {
            serde_json::json!({
                "from": format!("{}-{}", e.from.0.cpn(), e.from.1),
                "to": format!("{}-{}", e.to.0.cpn(), e.to.1),
                "class": class_str(e.class),
            })
        })
        .collect();

    let out = serde_json::json!({
        "packages": packages,
        "edges": dep_edges,
    });

    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
