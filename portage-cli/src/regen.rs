use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use portage_repo::{RegenOpts, ReposConf, Repository, SourceOpts, regen_cache};

/// Regenerate metadata caches.
///
/// With no repos named, every `repos.conf` repo *except the main one* is
/// regenerated — overlays are the repos that usually ship no `md5-cache`,
/// while the main repo's rsync/git cache is maintained upstream (name it
/// explicitly to regenerate it anyway). Each argument may be a repos.conf
/// name or a path.
///
/// Output goes to the repo's own `metadata/md5-cache` when writable,
/// otherwise to the user-side cache (`~/.cache/em/md5-cache/<repo>`) that
/// `em` consults for overlay metadata.
pub async fn run(
    repos_args: &[String],
    main_repo_path: &str,
    repos_dir: Option<&str>,
    output: Option<PathBuf>,
    jobs: Option<usize>,
    dedup: bool,
) -> Result<()> {
    let conf = ReposConf::load().ok();

    // Resolve targets: explicit names/paths, or every non-main conf repo.
    let mut targets: Vec<PathBuf> = Vec::new();
    if repos_args.is_empty() {
        let Some(conf) = &conf else {
            bail!("no repos named and no repos.conf found");
        };
        let main = conf.main_repo().map(|m| m.location.clone());
        for entry in conf.repos() {
            if Some(&entry.location) != main.as_ref() {
                targets.push(entry.location.clone());
            }
        }
        if targets.is_empty() {
            eprintln!("em regen: no overlays in repos.conf — nothing to do");
            return Ok(());
        }
    } else {
        for arg in repos_args {
            let path = Path::new(arg);
            if path.is_dir() {
                targets.push(path.to_path_buf());
            } else if let Some(entry) = conf.as_ref().and_then(|c| c.find(arg)) {
                targets.push(entry.location.clone());
            } else {
                bail!("'{arg}' is neither a directory nor a repos.conf repo name");
            }
        }
    }
    if output.is_some() && targets.len() > 1 {
        bail!("--output only makes sense with a single repository");
    }

    // Masters resolve against the main repo's parent (e.g. an overlay's
    // `masters = gentoo` → /var/db/repos/gentoo).
    let default_repos_dir = Path::new(main_repo_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let repos_dir = repos_dir.map_or(default_repos_dir, PathBuf::from);

    let mut total_errors = 0usize;
    for target in &targets {
        let (repo, masters) =
            Repository::open_with_masters(target.clone(), &repos_dir).context("open repo")?;

        let ebuilds: Vec<_> = repo
            .ebuilds_with_masters(&masters)
            .context("list ebuilds")?
            .into_iter()
            .collect();

        let out = match &output {
            Some(o) => o.clone(),
            None => {
                let own = repo.path().join("metadata/md5-cache");
                if std::fs::create_dir_all(&own).is_ok() {
                    own.into_std_path_buf()
                } else {
                    let user = user_cache_dir(repo.name());
                    eprintln!(
                        "em regen: {} is not writable, writing to {}",
                        repo.path(),
                        user.display()
                    );
                    user
                }
            }
        };

        eprintln!("regen ::{} ({} ebuilds)", repo.name(), ebuilds.len());
        let opts = RegenOpts {
            source: SourceOpts { jobs, dedup },
            output_dir: Some(out),
        };
        let stats = regen_cache(&repo, &masters, ebuilds, &opts, |done, total| {
            eprint!("\r[{done}/{total}]");
        })
        .await
        .context("regen")?;
        eprintln!();
        if stats.errors > 0 {
            eprintln!("::{}: {} sourcing errors", repo.name(), stats.errors);
            total_errors += stats.errors;
        }
    }

    if total_errors > 0 {
        bail!("{total_errors} sourcing errors");
    }
    Ok(())
}

/// `$XDG_CACHE_HOME/em/md5-cache/<repo>` (or `~/.cache/...`) — the cache
/// `em`'s overlay metadata loading reads.
fn user_cache_dir(repo_name: &str) -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".cache"))
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("em/md5-cache")
        .join(repo_name)
}
