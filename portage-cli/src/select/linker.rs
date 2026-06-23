//! `em select linker` — linker profile selection.
//!
//! Manages linker profile selection. Similar to binutils-config but focused
//! specifically on the linker (ld, lld, mold, etc.). Supports grouping profiles
//! by target architecture and showing which is active per architecture.

use std::collections::BTreeMap;
use std::io::Write as _;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};

use super::config_portage_dir;
use crate::cli::{Cli, LinkerAction};
use crate::style::C_STAR;

/// Base directory for linker env.d files.
///
/// For system: /etc/env.d/linker
/// For --local (Prefix): ${EPREFIX}/etc/env.d/linker (e.g., ~/.gentoo/etc/env.d/linker)
/// For --prefix <DIR>: <DIR>/etc/env.d/linker
fn linker_env_d_dir(globals: &Cli) -> Utf8PathBuf {
    let config_portage = config_portage_dir(globals);

    // config_portage_dir returns ${EPREFIX}/etc/portage
    // env.d is a sibling directory: ${EPREFIX}/etc/env.d
    if let Some(parent) = config_portage.parent() {
        let config_env_dir = parent.join("env.d/linker");
        if config_env_dir.is_dir() {
            return config_env_dir;
        }
    }

    // Fall back to system location
    let system_dir = Utf8PathBuf::from("/etc/env.d/linker");
    if system_dir.is_dir() {
        return system_dir;
    }

    // If neither exists, return the config-root env.d location (will be created on first use)
    config_portage
        .parent()
        .unwrap_or(Utf8Path::new("/"))
        .join("env.d/linker")
}

/// Path to the current linker profile config file.
fn current_linker_config_path(globals: &Cli, target: &str) -> Utf8PathBuf {
    let system_path = Utf8PathBuf::from(format!("/etc/env.d/linker/config-{}", target));
    if system_path.is_file() {
        return system_path;
    }
    config_portage_dir(globals).join(format!("env.d/linker/config-{}", target))
}

/// Path to the global linker environment file.
fn global_linker_env_path(globals: &Cli) -> Utf8PathBuf {
    let system_path = Utf8PathBuf::from("/etc/env.d/06linker");
    if system_path.is_file() || system_path.parent().is_some_and(|p| p.is_dir()) {
        return system_path;
    }
    config_portage_dir(globals).join("env.d/06linker")
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

/// A linker profile with its target.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct LinkerProfile {
    name: String,
    target: String,
}

/// List all linker profiles, grouped by target.
fn list_all_linker_profiles(globals: &Cli) -> Result<BTreeMap<String, Vec<LinkerProfile>>> {
    let base_dir = linker_env_d_dir(globals);
    let mut profiles_by_target: BTreeMap<String, Vec<LinkerProfile>> = BTreeMap::new();

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
                .push(LinkerProfile {
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

/// Get the current linker profile for a target.
fn get_current_linker_profile(globals: &Cli, target: &str) -> Option<String> {
    let config_path = current_linker_config_path(globals, target);
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

/// Set the linker profile for a target.
fn set_linker_profile(globals: &Cli, target: &str, profile: &str) -> Result<()> {
    let config_path = current_linker_config_path(globals, target);
    let base_dir = linker_env_d_dir(globals);

    let profile_path = base_dir.join(profile);
    if !profile_path.is_file() {
        bail!("linker profile '{}' not found at {}", profile, profile_path);
    }

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent))?;
    }

    std::fs::write(&config_path, format!("CURRENT={}\n", profile))
        .with_context(|| format!("writing {}", config_path))?;

    let profile_content = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("reading {}", profile_path))?;

    let global_env_path = global_linker_env_path(globals);
    let mut env_content = String::from("# Autogenerated by 'em select linker'.\n");

    for line in profile_content.lines() {
        let line = line.trim();
        if line.starts_with("PATH=")
            || line.starts_with("LDPATH=")
            || line.starts_with("MANPATH=")
            || line.starts_with("INFOPATH=")
            || line.starts_with("ROOTPATH=")
            || line.starts_with("LD=")
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

pub fn run(action: &LinkerAction, globals: &Cli) -> Result<()> {
    let target = match action {
        LinkerAction::List { target, .. } | LinkerAction::Show { target, .. } => target
            .clone()
            .unwrap_or_else(|| get_chost(globals).unwrap_or_else(|_| "native".to_string())),
        LinkerAction::Set { target, .. } => target
            .clone()
            .unwrap_or_else(|| get_chost(globals).unwrap_or_else(|_| "native".to_string())),
    };

    match action {
        LinkerAction::List { .. } => list(globals),
        LinkerAction::Show { .. } => show(globals, &target),
        LinkerAction::Set { profile, .. } => set(globals, &target, profile),
    }
}

fn list(globals: &Cli) -> Result<()> {
    let profiles_by_target = list_all_linker_profiles(globals)?;
    let mut out = anstream::stdout();

    if profiles_by_target.is_empty() {
        println!("No linker profiles found");
        return Ok(());
    }

    // Collect all profiles across all targets to calculate total count
    let all_profiles: Vec<&LinkerProfile> = profiles_by_target
        .values()
        .flat_map(|profiles| profiles.iter())
        .collect();
    let total_count = all_profiles.len();
    let num_width = total_count.to_string().len();

    let mut n = 1;
    let mut first = true;
    for (target, profiles) in &profiles_by_target {
        let current = get_current_linker_profile(globals, target);

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
    match get_current_linker_profile(globals, target) {
        Some(profile) => println!("{}", profile),
        None => println!("(no linker profile set for target '{}')", target),
    }
    Ok(())
}

fn set(globals: &Cli, target: &str, profile: &str) -> Result<()> {
    let profiles_by_target = list_all_linker_profiles(globals)?;

    let resolved_profile = if let Ok(n) = profile.parse::<usize>() {
        let mut all_profiles: Vec<&LinkerProfile> = profiles_by_target
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

    set_linker_profile(globals, target, &resolved_profile)?;
    println!(">>> linker profile set: {}", resolved_profile);
    println!("    for target: {}", target);

    Ok(())
}
