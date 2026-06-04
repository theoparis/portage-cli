use portage_atom::interner::Interned;
use portage_atom::Dep;
use portage_atom_pubgrub::{UseConfig, UseFlagState};
use portage_repo::{ProfileStack, Repository, DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF};

pub(super) async fn build_use_config(
    repo: &Repository,
) -> (UseConfig, Vec<String>, Vec<(Dep, Vec<String>)>) {
    let Some((use_str, use_expand, package_use)) = compute_use_env(repo).await else {
        return (UseConfig::new(), Vec::new(), Vec::new());
    };
    let mut config = UseConfig::new();
    for token in use_str.split_whitespace() {
        if let Some(name) = token.strip_prefix('-') {
            config.set(Interned::intern(name), UseFlagState::Disabled);
        } else {
            config.set(Interned::intern(token), UseFlagState::Enabled);
        }
    }
    (config, use_expand, package_use)
}

async fn compute_use_env(
    repo: &Repository,
) -> Option<(String, Vec<String>, Vec<(Dep, Vec<String>)>)> {
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
    let use_expand: Vec<String> = shell
        .get_var("USE_EXPAND")
        .unwrap_or_default()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    let mut package_use = stack.package_use().unwrap_or_default();
    package_use.extend(load_package_use("/etc/portage/package.use"));

    Some((use_str, use_expand, package_use))
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
