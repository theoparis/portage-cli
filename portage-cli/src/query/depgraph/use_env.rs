use portage_atom::Dep;
use portage_atom_pubgrub::{UseConfig, UseFlagState};
use portage_repo::{ProfileStack, Repository, DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF};

/// Resolved USE environment for the solver and display.
pub(super) struct UseEnv {
    pub config: UseConfig,
    /// Keys from `USE_EXPAND` — used to group expanded flags in display.
    pub expand: Vec<String>,
    /// Keys from `USE_EXPAND_HIDDEN` — groups to suppress in display.
    pub expand_hidden: Vec<String>,
    /// Per-package USE flag overrides from the profile and `/etc/portage/package.use`.
    pub package_use: Vec<(Dep, Vec<String>)>,
}

pub(super) async fn build_use_env(repo: &Repository) -> UseEnv {
    let Some(env) = compute_use_env(repo).await else {
        return UseEnv {
            config: UseConfig::new(),
            expand: Vec::new(),
            expand_hidden: Vec::new(),
            package_use: Vec::new(),
        };
    };
    env
}

async fn compute_use_env(repo: &Repository) -> Option<UseEnv> {
    let profile_path = std::fs::canonicalize("/etc/portage/make.profile").ok()?;
    let stack = ProfileStack::build(profile_path).ok()?;
    let mut shell = repo.shell().await.ok()?;

    let make_conf: Option<std::path::PathBuf> = [DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF]
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(std::path::PathBuf::from);

    let confs: Vec<&std::path::Path> = make_conf.as_deref().into_iter().collect();
    let flags = stack.use_flags(&mut shell, &confs).await.ok()?;

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

    let mut config = UseConfig::new();
    for flag in flags {
        config.set(flag, UseFlagState::Enabled);
    }

    let mut package_use = stack.package_use().unwrap_or_default();
    package_use.extend(load_package_use("/etc/portage/package.use"));

    Some(UseEnv { config, expand, expand_hidden, package_use })
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
