use camino::Utf8Path;
use portage_atom::Dep;
use portage_atom_pubgrub::{UseConfig, UseFlagState};
use portage_repo::{AcceptLicense, LicenseGroupRegistry, ProfileStack, Repository};

use super::force_mask::{ForceMask, index_by_cpn};

type Result<T> = anyhow::Result<T>;

/// Resolved USE environment for the solver and display.
pub(super) struct UseEnv {
    pub config: UseConfig,
    /// Keys from `USE_EXPAND` — used to group expanded flags in display.
    pub expand: Vec<String>,
    /// Keys from `USE_EXPAND_HIDDEN` — groups to suppress in display.
    pub expand_hidden: Vec<String>,
    /// Per-package USE flag overrides from the profile and `/etc/portage/package.use`.
    pub package_use: Vec<(Dep, Vec<String>)>,
    /// Masked packages: repo-global `profiles/package.mask`, the profile
    /// stack, and `/etc/portage/package.mask`.
    pub package_mask: Vec<Dep>,
    /// Site unmasks from `/etc/portage/package.unmask` — a matching entry
    /// cancels any mask for that package.
    pub package_unmask: Vec<Dep>,
    /// Profile USE force/mask policy (global + per-package + stable variants),
    /// applied per package to effective USE and consulted by the Level-C cede gate.
    pub force_mask: ForceMask,
    /// Effective ACCEPT_KEYWORDS tokens (e.g. `["arm64", "~arm64"]`).
    pub accept_keywords: Vec<String>,
    /// Effective `ACCEPT_LICENSE` after `@GROUP` expansion and `-` denials.
    pub accept_license: AcceptLicense,
    /// Resolved `DISTDIR` (where fetched distfiles live), for download-size accounting.
    pub distdir: String,
}

pub(super) async fn build_use_env(
    repo: &Repository,
    root: Option<&Utf8Path>,
    config_overlay: Option<&Utf8Path>,
) -> Result<UseEnv> {
    compute_use_env(repo, root, config_overlay).await
}

async fn compute_use_env(
    repo: &Repository,
    root: Option<&Utf8Path>,
    config_overlay: Option<&Utf8Path>,
) -> Result<UseEnv> {
    let portage_dir = root.unwrap_or(Utf8Path::new("/")).join("etc/portage");
    let root_dir = root.unwrap_or(Utf8Path::new("/"));

    let profile_link = portage_dir.join("make.profile");
    let profile_path = std::fs::canonicalize(profile_link.as_std_path())
        .map_err(|e| anyhow::anyhow!("cannot resolve {profile_link}: {e}"))?;
    // Portage appends `/etc/portage/profile` as the top (highest-priority)
    // profile layer, so its use.force/use.mask/package.use*/package.mask override
    // the resolved make.profile chain. Fold it in so Level-C never cedes a flag a
    // site override pins and the plan honours site masks (portage(5)).
    let stack = ProfileStack::build(profile_path)
        .map_err(|e| anyhow::anyhow!("failed to build profile stack: {e}"))?
        .with_user_profile(portage_dir.join("profile").into_std_path_buf())
        .map_err(|e| anyhow::anyhow!("failed to load /etc/portage/profile: {e}"))?;
    let mut shell = repo
        .shell()
        .await
        .map_err(|e| anyhow::anyhow!("failed to start ebuild shell: {e}"))?;

    let make_conf_candidates = [
        root_dir.join("etc/portage/make.conf"),
        root_dir.join("etc/make.conf"),
    ];
    let confs: Vec<&std::path::Path> = make_conf_candidates
        .iter()
        .filter(|p| p.as_std_path().exists())
        .map(|p| p.as_std_path())
        .collect();

    let flags = stack
        .use_flags(&mut shell, &confs)
        .await
        .map_err(|e| anyhow::anyhow!("failed to evaluate USE flags: {e}"))?;

    let split_var = |name: &str| -> Vec<String> {
        shell
            .get_var(name)
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    };

    let expand = split_var("USE_EXPAND");
    let expand_hidden = split_var("USE_EXPAND_HIDDEN");
    let accept_keywords = effective_accept_keywords(&split_var, &shell);
    let license_groups = LicenseGroupRegistry::from_repo(repo)
        .map_err(|e| anyhow::anyhow!("failed to load license groups: {e}"))?;
    let accept_license_tokens = {
        let from_profile = {
            let v = split_var("ACCEPT_LICENSE");
            if v.is_empty() {
                vec!["*".to_string()]
            } else {
                v
            }
        };
        // Portage honours ACCEPT_LICENSE from the process environment.
        match std::env::var("ACCEPT_LICENSE") {
            Ok(env) if !env.is_empty() => env.split_whitespace().map(str::to_string).collect(),
            _ => from_profile,
        }
    };
    let accept_license = AcceptLicense::from_tokens(&accept_license_tokens, &license_groups);
    let distdir = shell
        .get_var("DISTDIR")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/var/cache/distfiles".to_string());

    let mut config = UseConfig::new();
    for flag in flags {
        config.set(flag, UseFlagState::Enabled);
    }

    let mut package_use = stack.package_use().unwrap_or_default();
    package_use.extend(load_package_use(portage_dir.join("package.use").as_str()));
    // User config overlay (e.g. `--local`'s ~/.gentoo/etc/portage), applied
    // last so it overrides per flag — lets an unprivileged user set package.use
    // without writing the host /etc/portage.
    if let Some(overlay) = config_overlay {
        package_use.extend(load_package_use(overlay.join("package.use").as_str()));
    }

    // Mask sources, in portage's order: the repo-global `profiles/package.mask`
    // (applies regardless of profile), the profile chain (with `-atom` removals
    // already resolved), and the site `/etc/portage/package.mask`. A mask is
    // overridden per package by `/etc/portage/package.unmask`.
    let mut package_mask = repo.repo_package_mask().unwrap_or_default();
    package_mask.extend(stack.package_mask().unwrap_or_default());
    package_mask.extend(load_dep_list(portage_dir.join("package.mask").as_str()));
    let package_unmask = load_dep_list(portage_dir.join("package.unmask").as_str());

    // Profile USE force/mask. Global use.force/use.mask are already folded into
    // `config` by resolve_use_flags; we keep them here for the Level-C cede gate.
    // The package-level and *.stable.* sets are applied per package (force_mask.rs);
    // they carry raw `-flag` tokens so unforce/unmask is resolved per package.
    let force_mask = ForceMask {
        use_force: stack.use_force().unwrap_or_default(),
        use_mask: stack.use_mask().unwrap_or_default(),
        use_stable_force: stack.use_stable_force().unwrap_or_default(),
        use_stable_mask: stack.use_stable_mask().unwrap_or_default(),
        pkg_force: index_by_cpn(stack.package_use_force().unwrap_or_default()),
        pkg_mask: index_by_cpn(stack.package_use_mask().unwrap_or_default()),
        pkg_stable_force: index_by_cpn(stack.package_use_stable_force().unwrap_or_default()),
        pkg_stable_mask: index_by_cpn(stack.package_use_stable_mask().unwrap_or_default()),
    };

    Ok(UseEnv {
        config,
        expand,
        expand_hidden,
        package_use,
        package_mask,
        package_unmask,
        force_mask,
        accept_keywords,
        accept_license,
        distdir,
    })
}

/// `make.conf` often sets `ACCEPT_KEYWORDS="${ARCH} ~${ARCH}"`. When `ARCH` is
/// not yet visible at source time, brush leaves `~` only — rebuild from `ARCH`
/// once the profile stack has settled.
fn effective_accept_keywords(
    split_var: &dyn Fn(&str) -> Vec<String>,
    shell: &portage_repo::EbuildShell,
) -> Vec<String> {
    let arch = shell.get_var("ARCH").unwrap_or_default();
    let ak = split_var("ACCEPT_KEYWORDS");
    if arch.is_empty() {
        return ak;
    }
    let testing = format!("~{arch}");
    let has_arch = ak.iter().any(|k| k == &arch || k == &testing);
    if has_arch {
        return ak;
    }
    // Broken expansion (e.g. `["~"]` or empty) — mirror portage's make.conf default.
    vec![arch, testing]
}

fn load_package_use(path: &str) -> Vec<(Dep, Vec<String>)> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Vec::new();
    }
    let files: Vec<_> = if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        v.sort();
        v
    } else {
        vec![p.to_path_buf()]
    };
    let mut result = Vec::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(atom_str) = parts.next() else {
                continue;
            };
            let Ok(dep) = Dep::parse(atom_str) else {
                continue;
            };
            let flags: Vec<String> = parts.map(String::from).collect();
            if !flags.is_empty() {
                result.push((dep, flags));
            }
        }
    }
    result
}

/// Load a simple atom list (one dep per line, `#` comments, optionally a directory).
pub(super) fn load_dep_list(path: &str) -> Vec<Dep> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Vec::new();
    }
    let files: Vec<_> = if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        v.sort();
        v
    } else {
        vec![p.to_path_buf()]
    };
    let mut result = Vec::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // Strip leading '-' for incremental removal entries — skip removals here
            if line.starts_with('-') {
                continue;
            }
            if let Ok(dep) = Dep::parse(line) {
                result.push(dep);
            }
        }
    }
    result
}
