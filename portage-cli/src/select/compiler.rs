//! `em select compiler` — `gcc-config`/`eselect gcc` workalike.
//!
//! Manages compiler profile selection for gcc. Reads/writes env.d files and
//! creates symlinks similar to gcc-config. Supports grouping profiles by target
//! architecture and showing which is active per architecture.

use std::collections::BTreeMap;
use std::io::Write as _;

use anyhow::{Context, Result, bail};
use camino::Utf8PathBuf;

use super::config_portage_dir;
use crate::cli::{Cli, CompilerAction};
use crate::style::C_STAR;

/// Base directory for gcc env.d files.
fn gcc_env_d_dir(globals: &Cli) -> Utf8PathBuf {
    // First check config-root location (respects --config-root, --local, --prefix)
    let config_portage = config_portage_dir(globals);
    if let Some(parent) = config_portage.parent() {
        let config_env_dir = parent.join("env.d/gcc");
        if config_env_dir.is_dir() {
            return config_env_dir;
        }
    }
    // Fall back to system location
    let system_dir = Utf8PathBuf::from("/etc/env.d/gcc");
    if system_dir.is_dir() {
        return system_dir;
    }
    // Fall back to config-root env.d
    config_portage.join("env.d/gcc")
}

/// Path to the current compiler profile config file.
fn current_gcc_config_path(globals: &Cli, target: &str) -> Utf8PathBuf {
    let system_path = Utf8PathBuf::from(format!("/etc/env.d/gcc/config-{}", target));
    if system_path.is_file() {
        return system_path;
    }
    config_portage_dir(globals).join(format!("env.d/gcc/config-{}", target))
}

/// Path to the global gcc environment file.
fn global_gcc_env_path(globals: &Cli, target: &str) -> Utf8PathBuf {
    let system_path = Utf8PathBuf::from(format!("/etc/env.d/04gcc-{}", target));
    if system_path.is_file() || system_path.parent().is_some_and(|p| p.is_dir()) {
        return system_path;
    }
    config_portage_dir(globals).join(format!("env.d/04gcc-{}", target))
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

/// A gcc profile with its target.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct GccProfile {
    name: String,
    target: String,
}

/// List all gcc profiles, grouped by target.
fn list_all_gcc_profiles(globals: &Cli) -> Result<BTreeMap<String, Vec<GccProfile>>> {
    let base_dir = gcc_env_d_dir(globals);
    let mut profiles_by_target: BTreeMap<String, Vec<GccProfile>> = BTreeMap::new();

    if base_dir.is_dir() {
        for entry in std::fs::read_dir(&base_dir)? {
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

            // Read the profile to get CTARGET
            let target: Option<String> = {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let mut found = None;
                    for line in content.lines() {
                        let line = line.trim();
                        if line.starts_with("CTARGET=") {
                            let mut target = line.trim_start_matches("CTARGET=").trim().to_string();
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

            // If no CTARGET found, try to extract from profile name
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
                .push(GccProfile {
                    name: name.clone(),
                    target: profile_target,
                });
        }
    }

    for profiles in profiles_by_target.values_mut() {
        profiles.sort_by(|a, b| a.name.cmp(&b.name));
    }

    Ok(profiles_by_target)
}

/// Get the current gcc profile for a target.
fn get_current_gcc_profile(globals: &Cli, target: &str) -> Option<String> {
    let config_path = current_gcc_config_path(globals, target);
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

/// Set the gcc profile for a target.
fn set_gcc_profile(globals: &Cli, target: &str, profile: &str) -> Result<()> {
    let config_path = current_gcc_config_path(globals, target);
    let base_dir = gcc_env_d_dir(globals);

    let profile_path = base_dir.join(profile);
    if !profile_path.is_file() {
        bail!("gcc profile '{}' not found at {}", profile, profile_path);
    }

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent))?;
    }

    std::fs::write(&config_path, format!("CURRENT={}\n", profile))
        .with_context(|| format!("writing {}", config_path))?;

    let profile_content = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("reading {}", profile_path))?;

    let global_env_path = global_gcc_env_path(globals, target);
    let mut env_content = String::from("# Autogenerated by 'em select compiler'.\n");

    for line in profile_content.lines() {
        let line = line.trim();
        if line.starts_with("PATH=")
            || line.starts_with("LDPATH=")
            || line.starts_with("MANPATH=")
            || line.starts_with("INFOPATH=")
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

pub fn run(action: &CompilerAction, globals: &Cli) -> Result<()> {
    let target = match action {
        CompilerAction::List { target, .. } | CompilerAction::Show { target, .. } => target
            .clone()
            .unwrap_or_else(|| get_chost(globals).unwrap_or_else(|_| "native".to_string())),
        CompilerAction::Set { target, .. } => target
            .clone()
            .unwrap_or_else(|| get_chost(globals).unwrap_or_else(|_| "native".to_string())),
    };

    match action {
        CompilerAction::List { .. } => list(globals),
        CompilerAction::Show { .. } => show(globals, &target),
        CompilerAction::Set { profile, .. } => set(globals, &target, profile),
    }
}

fn list(globals: &Cli) -> Result<()> {
    let profiles_by_target = list_all_gcc_profiles(globals)?;
    let mut out = anstream::stdout();

    if profiles_by_target.is_empty() {
        println!("No gcc profiles found");
        return Ok(());
    }

    // Collect all profiles across all targets to calculate total count
    let all_profiles: Vec<&GccProfile> = profiles_by_target
        .values()
        .flat_map(|profiles| profiles.iter())
        .collect();
    let total_count = all_profiles.len();
    let num_width = total_count.to_string().len();

    let mut n = 1;
    let mut first = true;
    for (target, profiles) in &profiles_by_target {
        let current = get_current_gcc_profile(globals, target);

        if !first {
            writeln!(out).ok();
        }
        first = false;

        for profile in profiles {
            let is_current = current.as_deref() == Some(&profile.name);
            let num = format!("[{:>width$}]", n, width = num_width);
            let profile_display = if is_current {
                format!("{}{C_STAR} *{C_STAR:#}", profile.name)
            } else {
                profile.name.clone()
            };
            writeln!(out, "  {num} {}", profile_display).ok();
            n += 1;
        }
    }

    Ok(())
}

fn show(globals: &Cli, target: &str) -> Result<()> {
    match get_current_gcc_profile(globals, target) {
        Some(profile) => println!("{}", profile),
        None => println!("(no gcc profile set for target '{}')", target),
    }
    Ok(())
}

fn set(globals: &Cli, target: &str, profile: &str) -> Result<()> {
    let profiles_by_target = list_all_gcc_profiles(globals)?;

    let resolved_profile = if let Ok(n) = profile.parse::<usize>() {
        let mut all_profiles: Vec<&GccProfile> = profiles_by_target
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

    set_gcc_profile(globals, target, &resolved_profile)?;
    println!(">>> gcc profile set: {}", resolved_profile);
    println!("    for target: {}", target);

    Ok(())
}
