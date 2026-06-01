use std::collections::HashSet;

use camino::{Utf8Path, Utf8PathBuf};
use portage_distfiles::{DistfileResolver, FetchConfig, FetchStatus, Fetcher};
use portage_metadata::SrcUriEntry;
use portage_repo::{Ebuild, MakeConf, Manifest, Repository, DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF};

use crate::error::{Error, Result};

/// Execute one or more ebuild phases for a given `.ebuild` file.
pub async fn run(
    ebuild_path: &str,
    phases: &[String],
    work_dir: Option<&Utf8Path>,
    repo_override: Option<&str>,
) -> Result<()> {
    let path = Utf8Path::new(ebuild_path);
    let ebuild = Ebuild::from_path(path)
        .map_err(|e| Error::Other(format!("loading {ebuild_path}: {e}")))?;

    let repo_root = match repo_override {
        Some(r) => Utf8PathBuf::from(r),
        None => ebuild
            .repo_root()
            .ok_or_else(|| Error::Other("cannot determine repo root from ebuild path".into()))?
            .to_owned(),
    };

    let repo = Repository::open(repo_root.as_std_path())
        .map_err(|e| Error::Other(format!("opening repo at {repo_root}: {e}")))?;

    let work_root = match work_dir {
        Some(p) => p.to_owned(),
        None => {
            let pf = format!("{}-{}", ebuild.name(), ebuild.version());
            Utf8PathBuf::from(format!(
                "/var/tmp/portage/{}/{pf}",
                ebuild.category()
            ))
        }
    };

    let mut shell = repo
        .shell()
        .await
        .map_err(|e| Error::Other(format!("creating shell: {e}")))?;

    // Apply global USE flags from make.conf if available.
    if let Some(use_val) = read_use_from_make_conf() {
        let flags: Vec<&str> = use_val.split_whitespace().collect();
        shell
            .set_use_flags(&flags)
            .map_err(|e| Error::Other(format!("setting USE flags: {e}")))?;
    }

    for phase in phases {
        run_one_phase(&mut shell, &ebuild, &repo, phase, &work_root).await?;
    }

    Ok(())
}

async fn run_one_phase(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    repo: &Repository,
    phase: &str,
    work_root: &Utf8Path,
) -> Result<()> {
    match phase.as_ref() {
        "fetch" => run_fetch_stub(shell, ebuild, repo, work_root).await,
        "clean" => run_clean(work_root),
        "merge" | "qmerge" => {
            eprintln!("em ebuild: '{phase}' is not yet implemented");
            Err(Error::NotImplemented(format!("ebuild {phase}")))
        }
        _ => shell
            .run_phase(ebuild, phase, work_root.as_std_path())
            .await
            .map_err(|e| Error::Other(format!("phase {phase} failed: {e}"))),
    }
}

async fn run_fetch_stub(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    repo: &Repository,
    work_root: &Utf8Path,
) -> Result<()> {
    // Source the ebuild to populate SRC_URI, then compute $A.
    let sourced = shell
        .source_ebuild(ebuild)
        .await
        .map_err(|e| Error::Other(format!("sourcing ebuild: {e}")))?;
    shell.set_a_from_src_uri();

    let src_uri_str = shell.get_var("SRC_URI").unwrap_or_default();
    let distdir = Utf8PathBuf::from(
        shell.get_var("DISTDIR").unwrap_or_else(|| "/var/cache/distfiles".into()),
    );

    if src_uri_str.trim().is_empty() {
        println!("fetch: nothing to fetch (SRC_URI is empty)");
        return Ok(());
    }

    let entries = SrcUriEntry::parse(&src_uri_str)
        .map_err(|e| Error::Other(format!("parsing SRC_URI: {e}")))?;

    let use_flags: HashSet<String> = shell
        .get_var("USE")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();

    // Build GENTOO_MIRRORS from environment or make.conf.
    let gentoo_mirrors = gentoo_mirrors_list();

    let resolver = DistfileResolver::from_repo(repo, gentoo_mirrors)
        .map_err(|e| Error::Other(format!("loading mirrors: {e}")))?;
    let distfiles = resolver.resolve(&entries, &use_flags);

    if distfiles.is_empty() {
        println!("fetch: nothing to fetch");
        return Ok(());
    }

    // Load Manifest for verification.
    let manifest_path = ebuild.path()
        .parent()
        .map(|p| p.join("Manifest"))
        .filter(|p| p.exists());
    let manifest = match manifest_path {
        Some(ref p) => {
            let raw = std::fs::read_to_string(p)
                .map_err(|e| Error::Other(format!("reading Manifest: {e}")))?;
            Manifest::parse(&raw)
                .map_err(|e| Error::Other(format!("parsing Manifest: {e}")))?
        }
        None => Manifest { entries: vec![] },
    };

    // Read FETCHCOMMAND / RESUMECOMMAND from make.conf if set.
    let (fetch_cmd, resume_cmd) = read_fetch_commands();
    let config = FetchConfig::from_make_conf(fetch_cmd, resume_cmd);
    let fetcher = Fetcher::new(distdir.clone(), config);

    std::fs::create_dir_all(distdir.as_std_path())
        .map_err(|e| Error::Other(format!("creating distdir {distdir}: {e}")))?;

    let results = fetcher.fetch_all(&distfiles, &manifest).await;

    let mut any_failed = false;
    let mut any_restricted = false;
    for (df, result) in results {
        match result {
            Ok(FetchStatus::AlreadyPresent) => println!("fetch: {} (already present)", df.filename),
            Ok(FetchStatus::Downloaded) => println!("fetch: {} ok", df.filename),
            Ok(FetchStatus::FetchRestricted) => {
                eprintln!("fetch: {} is fetch-restricted (RESTRICT=fetch)", df.filename);
                any_restricted = true;
            }
            Err(e) => {
                eprintln!("fetch: {} failed: {e}", df.filename);
                any_failed = true;
            }
        }
    }

    // Suppress unused-variable warning on sourced metadata.
    let _ = sourced;

    // Run pkg_nofetch whenever we can't auto-fetch — either due to RESTRICT=fetch
    // or because all download attempts failed. pkg_nofetch prints manual download
    // instructions; run_phase silently skips it if the function isn't defined.
    if any_restricted || any_failed {
        shell
            .run_phase(ebuild, "nofetch", work_root.as_std_path())
            .await
            .map_err(|e| Error::Other(format!("pkg_nofetch failed: {e}")))?;
    }

    if any_failed {
        Err(Error::Other("one or more distfiles could not be fetched".into()))
    } else {
        Ok(())
    }
}

/// Remove the work directory tree (`clean` phase equivalent).
fn run_clean(work_root: &Utf8Path) -> Result<()> {
    if work_root.exists() {
        std::fs::remove_dir_all(work_root).map_err(|e| {
            Error::Other(format!("cleaning {work_root}: {e}"))
        })?;
        println!("clean: removed {work_root}");
    } else {
        println!("clean: {work_root} does not exist, nothing to do");
    }
    Ok(())
}

/// Read GENTOO_MIRRORS — from environment first, then make.conf.
fn gentoo_mirrors_list() -> Vec<String> {
    if let Ok(val) = std::env::var("GENTOO_MIRRORS") {
        if !val.trim().is_empty() {
            return val.split_whitespace().map(str::to_owned).collect();
        }
    }
    for candidate in [DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF] {
        let p = Utf8Path::new(candidate);
        if p.exists() {
            if let Ok(mc) = MakeConf::load(p) {
                if let Some(val) = mc.get("GENTOO_MIRRORS") {
                    return val.split_whitespace().map(str::to_owned).collect();
                }
            }
        }
    }
    vec![]
}

/// Read FETCHCOMMAND and RESUMECOMMAND from make.conf.
fn read_fetch_commands() -> (Option<String>, Option<String>) {
    for candidate in [DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF] {
        let p = Utf8Path::new(candidate);
        if p.exists() {
            if let Ok(mc) = MakeConf::load(p) {
                let fetch = mc.get("FETCHCOMMAND").map(str::to_owned);
                let resume = mc.get("RESUMECOMMAND").map(str::to_owned);
                if fetch.is_some() || resume.is_some() {
                    return (fetch, resume);
                }
            }
        }
    }
    (None, None)
}

fn read_use_from_make_conf() -> Option<String> {
    for candidate in [DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF] {
        let p = Utf8Path::new(candidate);
        if p.exists() {
            if let Ok(mc) = MakeConf::load(p) {
                if let Some(val) = mc.get("USE") {
                    return Some(val.to_owned());
                }
            }
        }
    }
    None
}
