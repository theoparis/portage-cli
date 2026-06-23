//! `em select binutils` — `binutils-config`/`eselect binutils` workalike.
//!
//! Manages binutils profile selection. Supports grouping profiles by target
//! architecture and showing which is active per architecture.

use std::collections::BTreeMap;
use std::io::Write as _;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};

use super::config_portage_dir;
use crate::cli::{BinutilsAction, Cli};
use crate::style::{C_HOST, C_PREFIX, C_STAR};

/// Base directory for binutils env.d files.
///
/// For system: /etc/env.d/binutils
/// For --local (Prefix): ${EPREFIX}/etc/env.d/binutils (e.g., ~/.gentoo/etc/env.d/binutils)
/// For --prefix <DIR>: <DIR>/etc/env.d/binutils
fn binutils_env_d_dir(globals: &Cli) -> Utf8PathBuf {
    let config_portage = config_portage_dir(globals);

    // config_portage_dir returns ${EPREFIX}/etc/portage
    // env.d is a sibling directory: ${EPREFIX}/etc/env.d
    if let Some(parent) = config_portage.parent() {
        let config_env_dir = parent.join("env.d/binutils");
        if config_env_dir.is_dir() {
            return config_env_dir;
        }
    }

    // Fall back to system location
    let system_dir = Utf8PathBuf::from("/etc/env.d/binutils");
    if system_dir.is_dir() {
        return system_dir;
    }

    // If neither exists, return the config-root env.d location (will be created on first use)
    config_portage
        .parent()
        .unwrap_or(Utf8Path::new("/"))
        .join("env.d/binutils")
}

/// Path to the current binutils profile config file.
fn current_binutils_config_path(globals: &Cli, target: &str) -> Utf8PathBuf {
    let system_path = Utf8PathBuf::from(format!("/etc/env.d/binutils/config-{}", target));
    if system_path.is_file() {
        return system_path;
    }
    // config file is at ${EPREFIX}/etc/env.d/binutils/config-{target}
    config_portage_dir(globals)
        .parent()
        .unwrap_or(Utf8Path::new("/"))
        .join(format!("env.d/binutils/config-{}", target))
}

/// Path to the global binutils environment file (05binutils).
fn global_binutils_env_path(globals: &Cli) -> Utf8PathBuf {
    let system_path = Utf8PathBuf::from("/etc/env.d/05binutils");
    if system_path.is_file() || system_path.parent().is_some_and(|p| p.is_dir()) {
        return system_path;
    }
    // env file is at ${EPREFIX}/etc/env.d/05binutils
    config_portage_dir(globals)
        .parent()
        .unwrap_or(Utf8Path::new("/"))
        .join("env.d/05binutils")
}

/// Get CHOST from make.conf.
fn get_chost(globals: &Cli) -> Result<String> {
    let make_conf_path = config_portage_dir(globals).join("make.conf");
    let system_make_conf = Utf8PathBuf::from("/etc/make.conf");

    let paths_to_check = if system_make_conf.is_file() {
        vec![system_make_conf, make_conf_path]
    } else {
        vec![make_conf_path]
    };

    for path in paths_to_check {
        if let Ok(content) = std::fs::read_to_string(&path) {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with("CHOST=") {
                    let mut chost = line.trim_start_matches("CHOST=").trim().to_string();
                    let needs_strip = (chost.starts_with('"') && chost.ends_with('"'))
                        || (chost.starts_with("'") && chost.ends_with("'"));
                    if needs_strip {
                        chost = chost[1..chost.len() - 1].to_string();
                    }
                    return Ok(chost);
                }
            }
        }
    }
    let arch = globals.arch.as_str();
    Ok(format!("{arch}-unknown-linux-gnu"))
}

/// A binutils profile with its target.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct BinutilsProfile {
    name: String,
    target: String,
    /// Whether this profile is from the host system or the current config root
    is_host: bool,
}

/// List all binutils profiles, grouped by target.
/// When using --local or --prefix, also includes host system profiles
/// with a flag indicating their source.
fn list_all_binutils_profiles(globals: &Cli) -> Result<BTreeMap<String, Vec<BinutilsProfile>>> {
    let mut profiles_by_target: BTreeMap<String, Vec<BinutilsProfile>> = BTreeMap::new();

    // Check if we're in a prefix/local context
    let roots = globals.roots();
    let is_prefix_context = roots.config().is_none() && roots.config_overlay().is_some();

    // Collect profiles from the current config root (prefix/local)
    let prefix_base_dir = binutils_env_d_dir(globals);
    if prefix_base_dir.is_dir() {
        collect_binutils_profiles(&prefix_base_dir, &mut profiles_by_target, false)?;
    }

    // If in prefix context, also check system location
    if is_prefix_context {
        let system_dir = Utf8PathBuf::from("/etc/env.d/binutils");
        if system_dir.is_dir() {
            collect_binutils_profiles(&system_dir, &mut profiles_by_target, true)?;
        }
    }

    for profiles in profiles_by_target.values_mut() {
        profiles.sort_by(|a, b| a.name.cmp(&b.name));
    }

    Ok(profiles_by_target)
}

/// Helper to collect binutils profiles from a directory
fn collect_binutils_profiles(
    base_dir: &Utf8PathBuf,
    profiles_by_target: &mut BTreeMap<String, Vec<BinutilsProfile>>,
    is_host: bool,
) -> Result<()> {
    for entry in std::fs::read_dir(base_dir)? {
        let entry = entry?;
        let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        let name = path.file_name().unwrap_or_default().to_string();

        // Skip config files
        if name.starts_with("config-") {
            continue;
        }

        // Skip non-profile files
        if name.ends_with(".conf") || name.is_empty() || !name.contains(char::is_numeric) {
            continue;
        }

        // Read the profile to get TARGET
        let target: Option<String> = {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let mut found = None;
                for line in content.lines() {
                    let line = line.trim();
                    if line.starts_with("TARGET=") {
                        let mut target = line.trim_start_matches("TARGET=").trim().to_string();
                        let needs_strip = (target.starts_with('"') && target.ends_with('"'))
                            || (target.starts_with("'") && target.ends_with("'"));
                        if needs_strip {
                            target = target[1..target.len() - 1].to_string();
                        }
                        found = Some(target);
                        break;
                    }
                }
                found
            } else {
                None
            }
        };

        // If no TARGET found, try to extract from profile name
        let profile_target = target.unwrap_or_else(|| {
            if let Some(pos) = name.rfind('-') {
                let version_part = &name[pos + 1..];
                if version_part.chars().all(|c| c.is_ascii_digit() || c == '.') {
                    return name[..pos].to_string();
                }
            }
            name.clone()
        });

        profiles_by_target
            .entry(profile_target.clone())
            .or_default()
            .push(BinutilsProfile {
                name: name.clone(),
                target: profile_target,
                is_host,
            });
    }
    Ok(())
}

/// Get the current binutils profile for a target.
fn get_current_binutils_profile(globals: &Cli, target: &str) -> Option<String> {
    let config_path = current_binutils_config_path(globals, target);
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("CURRENT=") {
                return Some(line.trim_start_matches("CURRENT=").trim().to_string());
            }
        }
    }
    None
}

/// Set the binutils profile for a target.
fn set_binutils_profile(globals: &Cli, target: &str, profile: &str) -> Result<()> {
    let config_path = current_binutils_config_path(globals, target);
    let base_dir = binutils_env_d_dir(globals);

    let mut profile_path = base_dir.join(profile);

    if !profile_path.is_file() {
        let target_dir = base_dir.join(target);
        let target_profile_path = target_dir.join(profile);
        if target_profile_path.is_file() {
            profile_path = target_profile_path;
        }
    }

    if !profile_path.is_file() {
        bail!("binutils profile '{}' not found", profile);
    }

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent))?;
    }

    std::fs::write(&config_path, format!("CURRENT={}\n", profile))
        .with_context(|| format!("writing {}", config_path))?;

    let profile_content = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("reading {}", profile_path))?;

    let global_env_path = global_binutils_env_path(globals);
    let mut env_content = String::from("# Autogenerated by 'em select binutils'.\n");

    for line in profile_content.lines() {
        let line = line.trim();
        if line.starts_with("PATH=")
            || line.starts_with("LDPATH=")
            || line.starts_with("MANPATH=")
            || line.starts_with("INFOPATH=")
            || line.starts_with("ROOTPATH=")
        {
            env_content.push_str(line);
            env_content.push('\n');
        }
    }

    if let Some(parent) = global_env_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent))?;
    }
    std::fs::write(&global_env_path, env_content)
        .with_context(|| format!("writing {}", global_env_path))?;

    Ok(())
}

pub fn run(action: &BinutilsAction, globals: &Cli) -> Result<()> {
    let target = match action {
        BinutilsAction::List { target, .. } | BinutilsAction::Show { target, .. } => target
            .clone()
            .unwrap_or_else(|| get_chost(globals).unwrap_or_else(|_| "native".to_string())),
        BinutilsAction::Set { target, .. } => target
            .clone()
            .unwrap_or_else(|| get_chost(globals).unwrap_or_else(|_| "native".to_string())),
    };

    match action {
        BinutilsAction::List { .. } => list(globals),
        BinutilsAction::Show { .. } => show(globals, &target),
        BinutilsAction::Set { profile, .. } => set(globals, &target, profile),
    }
}

fn list(globals: &Cli) -> Result<()> {
    let profiles_by_target = list_all_binutils_profiles(globals)?;
    let mut out = anstream::stdout();

    if profiles_by_target.is_empty() {
        println!("No binutils profiles found");
        return Ok(());
    }

    // Collect all profiles across all targets to calculate total count
    let all_profiles: Vec<&BinutilsProfile> = profiles_by_target
        .values()
        .flat_map(|profiles| profiles.iter())
        .collect();
    let total_count = all_profiles.len();
    let num_width = total_count.to_string().len();

    let mut n = 1;
    let mut first = true;
    for (target, profiles) in &profiles_by_target {
        let current = get_current_binutils_profile(globals, target);

        if !first {
            writeln!(out).ok();
        }
        first = false;

        for profile in profiles {
            // For binutils, the CURRENT value might be just the version or the full name
            let is_current = if let Some(current_name) = &current {
                profile.name == *current_name
                    || profile.name.ends_with(&format!("-{}", current_name))
            } else {
                false
            };
            let num = format!("[{:>width$}]", n, width = num_width);
            let mut profile_display = if is_current {
                format!("{}{C_STAR} *{C_STAR:#}", profile.name)
            } else {
                profile.name.clone()
            };

            // Add source label if in prefix context
            let roots = globals.roots();
            let is_prefix_context = roots.config().is_none() && roots.config_overlay().is_some();
            if is_prefix_context {
                let label = if profile.is_host {
                    format!("{C_HOST} (host){C_HOST:#}")
                } else {
                    format!("{C_PREFIX} (prefix){C_PREFIX:#}")
                };
                profile_display.push_str(&label);
            }

            writeln!(out, "  {num} {}", profile_display).ok();
            n += 1;
        }
    }

    Ok(())
}

fn show(globals: &Cli, target: &str) -> Result<()> {
    match get_current_binutils_profile(globals, target) {
        Some(profile) => println!("{}", profile),
        None => println!("(no binutils profile set for target '{}')", target),
    }
    Ok(())
}

fn set(globals: &Cli, target: &str, profile: &str) -> Result<()> {
    let profiles_by_target = list_all_binutils_profiles(globals)?;

    let resolved_profile = if let Ok(n) = profile.parse::<usize>() {
        let mut all_profiles: Vec<&BinutilsProfile> = profiles_by_target
            .values()
            .flat_map(|profiles| profiles.iter())
            .collect();
        all_profiles.sort_by(|a, b| a.name.cmp(&b.name));

        let idx = n.checked_sub(1).context("profile numbers start at 1")?;
        all_profiles
            .get(idx)
            .with_context(|| {
                format!(
                    "profile number {} out of range (1..={})",
                    n,
                    all_profiles.len()
                )
            })?
            .name
            .clone()
    } else {
        profile.to_string()
    };

    set_binutils_profile(globals, target, &resolved_profile)?;
    println!(">>> binutils profile set: {}", resolved_profile);
    println!("    for target: {}", target);

    Ok(())
}
