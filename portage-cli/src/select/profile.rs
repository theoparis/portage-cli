//! `em select profile` — cross-aware `eselect profile` workalike.
//!
//! `list` / `show` / `set`. The cross-aware difference: `set` accepts **any**
//! profile path (or a list number) and links it *without* validating it against
//! the host architecture — `eselect profile` refuses a foreign-arch profile,
//! which is exactly what a cross sysroot needs (see `todo/crossdev-target.md`,
//! the profile-linking item). Target the sysroot with the global `--config-root`
//! (e.g. `em --config-root /usr/<CTARGET> select profile set <path>`).

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::{ReposConf, Repository};

use super::config_portage_dir;
use crate::cli::Cli;

pub fn run(args: &[String], globals: &Cli) -> Result<()> {
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" => list(globals),
        "show" => show(globals),
        "set" => {
            let target = args
                .get(1)
                .context("usage: em select profile set <number|path>")?;
            set(globals, target)
        }
        other => bail!("em select profile: unknown action '{other}' (list, show, set)"),
    }
}

/// The repo whose `profiles/` we list/link from — the configured main repo
/// (usually `gentoo`).
fn main_repo() -> Result<Repository> {
    let conf = ReposConf::load().context("reading repos.conf")?;
    let entry = conf
        .main_repo()
        .or_else(|| conf.find("gentoo"))
        .context("no main repo configured in repos.conf")?;
    Repository::open(&entry.location)
        .with_context(|| format!("opening main repo at {}", entry.location.display()))
}

/// Where `make.profile` lives for this invocation.
fn make_profile_link(globals: &Cli) -> Utf8PathBuf {
    config_portage_dir(globals).join("make.profile")
}

/// The profile path the current `make.profile` points at, relative to the repo's
/// `profiles/` dir (so it can be matched against `profiles.desc`).
fn current_profile(globals: &Cli, repo: &Repository) -> Option<String> {
    let link = make_profile_link(globals);
    // Canonicalize the link itself so a relative symlink (`../../var/db/…`, as
    // eselect writes) resolves against its own directory, not the CWD.
    let target = link.canonicalize_utf8().ok()?;
    let profiles = repo.path().join("profiles").canonicalize_utf8().ok()?;
    target.strip_prefix(&profiles).ok().map(Utf8Path::to_string)
}

fn list(globals: &Cli) -> Result<()> {
    let repo = main_repo()?;
    let descs = repo.profiles_desc().context("reading profiles.desc")?;
    let current = current_profile(globals, &repo);
    for (i, d) in descs.iter().enumerate() {
        let mark = if current.as_deref() == Some(d.path()) {
            " *"
        } else {
            ""
        };
        println!(
            "  [{}]   {}  ({}, {}){}",
            i + 1,
            d.path(),
            d.arch().as_str(),
            d.status(),
            mark
        );
    }
    Ok(())
}

fn show(globals: &Cli) -> Result<()> {
    let repo = main_repo()?;
    match current_profile(globals, &repo) {
        Some(p) => println!("{p}"),
        None => println!("(no profile set at {})", make_profile_link(globals)),
    }
    Ok(())
}

fn set(globals: &Cli, target: &str) -> Result<()> {
    let repo = main_repo()?;

    // Resolve a list number (1-based, as displayed) to a profile path; otherwise
    // treat the argument as a profile path directly (the cross-aware case — no
    // host-arch validation).
    let profile_path = if let Ok(n) = target.parse::<usize>() {
        let descs = repo.profiles_desc().context("reading profiles.desc")?;
        let idx = n.checked_sub(1).context("profile numbers start at 1")?;
        descs
            .get(idx)
            .with_context(|| format!("profile number {n} out of range (1..={})", descs.len()))?
            .path()
            .to_string()
    } else {
        target.to_string()
    };

    let profile_dir = repo.path().join("profiles").join(&profile_path);
    if !profile_dir.is_dir() {
        bail!("profile '{profile_path}' not found at {profile_dir}");
    }

    let link = make_profile_link(globals);
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {parent}"))?;
    }
    // Replace any existing symlink/file. Absolute target so it resolves the same
    // from a sysroot/offset (matching crossdev-stages, not eselect's relative
    // link) — important for cross sysroots.
    match std::fs::symlink_metadata(&link) {
        Ok(_) => {
            std::fs::remove_file(&link).with_context(|| format!("removing existing {link}"))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("stat {link}")),
    }
    std::os::unix::fs::symlink(&profile_dir, &link)
        .with_context(|| format!("linking {link} -> {profile_dir}"))?;

    println!(">>> profile set: {profile_path}");
    println!("    {link} -> {profile_dir}");
    Ok(())
}
