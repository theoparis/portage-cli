//! `em select repos` — `eselect repository` workalike for **local** repos.
//!
//! `list` / `add` / `remove` / `create`. Manages `repos.conf` entries and
//! (for `create`) lays down a local overlay skeleton on disk. Remote
//! repositories (`sync-type`/`sync-uri` and the online repository list) are a
//! TODO — see `todo/crossdev-target.md`.

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::ReposConf;

use super::config_portage_dir;
use crate::cli::{Cli, RepositoryAction};

pub fn run(action: &RepositoryAction, globals: &Cli) -> Result<()> {
    match action {
        RepositoryAction::List => list(globals),
        RepositoryAction::Add { name, location } => add(globals, name, Utf8Path::new(location)),
        RepositoryAction::Remove { name } => remove(globals, name),
        RepositoryAction::Create { name, location } => {
            create(globals, name, location.as_deref().map(Utf8Path::new))
        }
    }
}

/// `repos.conf` search paths for the active config root.
fn conf_paths(globals: &Cli) -> Vec<Utf8PathBuf> {
    let config_root = globals
        .roots()
        .config()
        .unwrap_or_else(|| Utf8Path::new("/"))
        .to_owned();
    vec![
        config_root.join("usr/share/portage/config/repos.conf"),
        config_portage_dir(globals).join("repos.conf"),
    ]
}

/// The per-repo conf file `em` writes/removes (`repos.conf/<name>.conf`).
fn repo_conf_file(globals: &Cli, name: &str) -> Utf8PathBuf {
    config_portage_dir(globals)
        .join("repos.conf")
        .join(format!("{name}.conf"))
}

fn list(globals: &Cli) -> Result<()> {
    // `Utf8PathBuf: AsRef<Path>`, so the camino paths feed `load_from` directly.
    let conf = ReposConf::load_from(&conf_paths(globals)).context("reading repos.conf")?;
    for r in conf.repos() {
        let main = if conf.main_repo().is_some_and(|m| m.name == r.name) {
            " (main)"
        } else {
            ""
        };
        println!("  {:<20} {}{}", r.name, r.location.display(), main);
    }
    Ok(())
}

/// The `repos.conf` stanza for a local repo (the `[name]` + `location` lines).
fn repo_conf_body(name: &str, location: &Utf8Path) -> String {
    format!("# created by `em select repos`\n[{name}]\nlocation = {location}\n")
}

/// Lay down a minimal PMS overlay skeleton at `location` (mirrors
/// `eselect repository create`): `profiles/repo_name` + `metadata/layout.conf`.
fn write_overlay_skeleton(location: &Utf8Path, name: &str) -> Result<()> {
    std::fs::create_dir_all(location.join("profiles"))
        .with_context(|| format!("creating {location}/profiles"))?;
    std::fs::create_dir_all(location.join("metadata"))
        .with_context(|| format!("creating {location}/metadata"))?;
    std::fs::write(location.join("profiles/repo_name"), format!("{name}\n"))
        .context("writing profiles/repo_name")?;
    std::fs::write(
        location.join("metadata/layout.conf"),
        "masters = gentoo\nthin-manifests = true\n",
    )
    .context("writing metadata/layout.conf")?;
    Ok(())
}

/// Write `repos.conf/<name>.conf` for a local repo at `location`.
fn write_conf(globals: &Cli, name: &str, location: &Utf8Path) -> Result<()> {
    let file = repo_conf_file(globals, name);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {parent}"))?;
    }
    std::fs::write(&file, repo_conf_body(name, location))
        .with_context(|| format!("writing {file}"))?;
    println!(">>> wrote {file}");
    Ok(())
}

fn add(globals: &Cli, name: &str, location: &Utf8Path) -> Result<()> {
    if !location.is_dir() {
        bail!(
            "location {location} does not exist — `add` registers an existing local repo; \
             use `create` to make a new one (remote sync is not supported yet)"
        );
    }
    let location = location
        .canonicalize_utf8()
        .with_context(|| format!("resolving {location}"))?;
    write_conf(globals, name, &location)?;
    println!(">>> added local repo '{name}' at {location}");
    Ok(())
}

fn remove(globals: &Cli, name: &str) -> Result<()> {
    let file = repo_conf_file(globals, name);
    match std::fs::remove_file(&file) {
        Ok(()) => {
            println!(">>> removed repo '{name}' ({file})");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "no `{file}` to remove — '{name}' may be defined in a shared repos.conf file \
             (e.g. eselect-repo.conf); edit that by hand"
        ),
        Err(e) => Err(e).with_context(|| format!("removing {file}")),
    }
}

fn create(globals: &Cli, name: &str, location: Option<&Utf8Path>) -> Result<()> {
    let config_root = globals
        .roots()
        .config()
        .unwrap_or_else(|| Utf8Path::new("/"))
        .to_owned();
    let location = location
        .map(Utf8Path::to_owned)
        .unwrap_or_else(|| config_root.join("var/db/repos").join(name));

    if location.join("profiles/repo_name").exists() {
        bail!("an overlay already exists at {location}");
    }

    write_overlay_skeleton(&location, name)?;
    println!(">>> created overlay skeleton at {location}");
    write_conf(globals, name, &location)?;
    println!(">>> created local repo '{name}' at {location}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    #[test]
    fn conf_body_is_a_valid_local_stanza() {
        let body = repo_conf_body("myov", Utf8Path::new("/srv/ov"));
        assert!(body.contains("[myov]\n"));
        assert!(body.contains("location = /srv/ov\n"));
        // local only — no remote sync keys
        assert!(!body.contains("sync-"));
    }

    #[test]
    fn overlay_skeleton_is_a_valid_pms_overlay() {
        let td = tempfile::TempDir::new().unwrap();
        let loc = Utf8PathBuf::from_path_buf(td.path().join("ov")).unwrap();
        write_overlay_skeleton(&loc, "ov").unwrap();
        assert_eq!(
            std::fs::read_to_string(loc.join("profiles/repo_name")).unwrap(),
            "ov\n"
        );
        let layout = std::fs::read_to_string(loc.join("metadata/layout.conf")).unwrap();
        assert!(layout.contains("masters = gentoo"));
    }
}
