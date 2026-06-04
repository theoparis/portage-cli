use portage_atom::Dep;
use portage_atom_pubgrub::{UseConfig, UseFlagState};
use portage_repo::{ProfileStack, Repository, DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF};

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
    /// Masked packages from the profile stack and `/etc/portage/package.mask`.
    pub package_mask: Vec<Dep>,
    /// Effective ACCEPT_KEYWORDS tokens (e.g. `["arm64", "~arm64"]`).
    pub accept_keywords: Vec<String>,
    /// Effective ACCEPT_LICENSE tokens (e.g. `["*"]` or `["MIT", "GPL-2"]`).
    pub accept_license: Vec<String>,
}

pub(super) async fn build_use_env(repo: &Repository) -> Result<UseEnv> {
    compute_use_env(repo).await
}

async fn compute_use_env(repo: &Repository) -> Result<UseEnv> {
    let profile_path = std::fs::canonicalize("/etc/portage/make.profile")
        .map_err(|e| anyhow::anyhow!("cannot resolve /etc/portage/make.profile: {e}"))?;
    let stack = ProfileStack::build(profile_path)
        .map_err(|e| anyhow::anyhow!("failed to build profile stack: {e}"))?;
    let mut shell = repo.shell().await
        .map_err(|e| anyhow::anyhow!("failed to start ebuild shell: {e}"))?;

    let make_conf: Option<std::path::PathBuf> = [DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF]
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(std::path::PathBuf::from);

    let confs: Vec<&std::path::Path> = make_conf.as_deref().into_iter().collect();
    let flags = stack.use_flags(&mut shell, &confs).await
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
    let accept_keywords = split_var("ACCEPT_KEYWORDS");
    let accept_license = {
        let v = split_var("ACCEPT_LICENSE");
        if v.is_empty() { vec!["*".to_string()] } else { v }
    };

    let mut config = UseConfig::new();
    for flag in flags {
        config.set(flag, UseFlagState::Enabled);
    }

    let mut package_use = stack.package_use().unwrap_or_default();
    package_use.extend(load_package_use("/etc/portage/package.use"));

    let mut package_mask = stack.package_mask().unwrap_or_default();
    package_mask.extend(load_dep_list("/etc/portage/package.mask"));

    Ok(UseEnv { config, expand, expand_hidden, package_use, package_mask, accept_keywords, accept_license })
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
