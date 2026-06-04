use std::collections::BTreeMap;

use anyhow::{Result, bail};
use portage_atom::{Cpn, Cpv};
use portage_metadata::RawCacheEntry;
use portage_repo::{CacheReadOpts, Repository, cache_entries_parallel};

pub async fn run(
    repo_paths: &[std::path::PathBuf],
    pattern: Option<&str>,
    all: bool,
    search_desc: bool,
    name_only: bool,
    homepage: bool,
) -> Result<()> {
    if repo_paths.is_empty() {
        bail!("no repositories configured");
    }
    let mut repos: Vec<Repository> = Vec::with_capacity(repo_paths.len());
    for p in repo_paths {
        match Repository::open(p) {
            Ok(r) => repos.push(r),
            Err(e) => eprintln!("em: skipping {}: {e}", p.display()),
        }
    }
    if repos.is_empty() {
        bail!("no usable repositories");
    }
    let pat = pattern.unwrap_or("");

    if search_desc {
        run_desc(&repos, pat, all, name_only, homepage).await
    } else {
        run_name(&repos, pat, all, name_only, homepage)
    }
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    if nb.len() > hb.len() {
        return false;
    }
    hb.windows(nb.len()).any(|w| w.eq_ignore_ascii_case(nb))
}

fn run_name(
    repos: &[Repository],
    pat: &str,
    all: bool,
    name_only: bool,
    homepage: bool,
) -> Result<()> {
    let pat_has_slash = pat.contains('/');
    let mut matched: BTreeMap<String, (Cpn, usize)> = BTreeMap::new();
    for (idx, repo) in repos.iter().enumerate() {
        for cat in repo.categories() {
            for pkg in cat.packages() {
                let hit = if all {
                    true
                } else if pat_has_slash {
                    let full = format!("{}/{}", cat.name(), pkg.name());
                    contains_ci(&full, pat)
                } else {
                    contains_ci(pkg.name(), pat)
                };
                if hit {
                    let key = format!("{}/{}", cat.name(), pkg.name());
                    matched.entry(key).or_insert_with(|| (*pkg.cpn(), idx));
                }
            }
        }
    }

    if name_only {
        for key in matched.keys() {
            println!("{key}");
        }
        return Ok(());
    }

    for (key, (cpn, idx)) in &matched {
        let info = latest_entry_info(&repos[*idx], cpn, homepage);
        println!("{key}: {info}");
    }
    Ok(())
}

fn latest_entry_info(repo: &Repository, cpn: &Cpn, homepage: bool) -> String {
    let Some(cat) = repo.category(cpn.category.as_str()) else {
        return String::new();
    };
    let Some(pkg) = cat.package(cpn.package.as_str()) else {
        return String::new();
    };
    let Ok(ebuilds) = pkg.ebuilds() else {
        return String::new();
    };
    let Some(latest) = ebuilds.last() else {
        return String::new();
    };
    match repo.cache_entry(latest.cpv()).ok().flatten() {
        Some(entry) if homepage => entry.metadata.homepage.join(" "),
        Some(entry) => entry.metadata.description,
        None => String::new(),
    }
}

async fn run_desc(
    repos: &[Repository],
    pat: &str,
    all: bool,
    name_only: bool,
    homepage: bool,
) -> Result<()> {
    let opts = CacheReadOpts {
        latest_per_cpn: true,
        ..Default::default()
    };

    let pat_owned = pat.to_string();
    let mut entries: Vec<(Cpv, Option<String>)> =
        cache_entries_parallel(repos, &opts, move |text| {
            let raw = RawCacheEntry::new(text);
            let desc = raw.field("DESCRIPTION").unwrap_or("");
            if !all && !contains_ci(desc, &pat_owned) {
                return Ok(None);
            }
            let info = if name_only {
                None
            } else if homepage {
                Some(raw.field("HOMEPAGE").unwrap_or("").to_string())
            } else {
                Some(desc.to_string())
            };
            Ok::<_, portage_repo::Error>(Some(info))
        })
        .await
        .into_iter()
        .filter_map(|(cpv, r)| r.ok().flatten().map(|info| (cpv, info)))
        .collect();

    entries.sort_by(|(a, _), (b, _)| {
        a.cpn
            .category
            .cmp(&b.cpn.category)
            .then_with(|| a.cpn.package.cmp(&b.cpn.package))
    });

    for (cpv, info) in &entries {
        let key = format!("{}/{}", cpv.cpn.category, cpv.cpn.package);
        match info {
            None => println!("{key}"),
            Some(s) => println!("{key}: {s}"),
        }
    }
    Ok(())
}
