use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use portage_repo::{RegenOpts, Repository, SourceOpts, regen_cache};

pub async fn run(
    repo_path: &str,
    repos_dir: Option<&str>,
    output: Option<PathBuf>,
    jobs: Option<usize>,
    dedup: bool,
) -> Result<()> {
    let (repo, masters) = if let Some(dir) = repos_dir {
        Repository::open_with_masters(repo_path, dir).context("open repo")?
    } else {
        let repo = Repository::open(repo_path).context("open repo")?;
        (repo, vec![])
    };

    let ebuilds: Vec<_> = repo
        .ebuilds()
        .context("list ebuilds")?
        .into_iter()
        .collect();

    let out = output.unwrap_or_else(|| repo.path().join("metadata/md5-cache").into_std_path_buf());

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
        bail!("{} sourcing errors", stats.errors);
    }
    Ok(())
}
