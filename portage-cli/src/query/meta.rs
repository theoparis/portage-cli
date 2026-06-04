//! `em query meta` — display package metadata from repo + VDB.

use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use humansize::{BINARY, format_size};
use portage_atom::Dep;
use portage_repo::Repository;
use portage_vdb::Vdb;

use super::which::dep_matches_cpv;
use crate::vdb::find_packages;

pub fn run(repo_path: &Path, vdb: Option<&Vdb>, atoms: &[String]) -> Result<()> {
    let repo = Repository::open(repo_path)?;

    let ebuilds: Vec<_> = repo.ebuilds()?.into_iter().collect();

    for raw in atoms {
        let dep = Dep::parse(raw).with_context(|| format!("bad atom '{raw}'"))?;

        let mut matches: Vec<_> = ebuilds
            .iter()
            .filter(|e| dep_matches_cpv(&dep, e.cpv()))
            .collect();

        if matches.is_empty() {
            eprintln!("em: no ebuild found for '{raw}'");
            continue;
        }

        matches.sort_by(|a, b| a.cpv().version.cmp(&b.cpv().version));
        let best = matches.last().unwrap();
        let cpv = best.cpv();

        let entry = repo
            .cache_entry(cpv)?
            .ok_or_else(|| anyhow!("no cache entry for {cpv} — run `em regen`"))?;
        let m = &entry.metadata;

        let pkg_meta = repo
            .category(cpv.cpn.category.as_ref())
            .and_then(|c| c.packages().into_iter().find(|p| p.name() == cpv.cpn.package.as_ref()))
            .and_then(|p| p.metadata_xml().ok().flatten());

        println!(" * {cpv}");

        if let Some(ref pm) = pkg_meta {
            for maint in &pm.maintainers {
                println!("   Maintainer:  {}", maint.display());
            }
        }

        if !m.homepage.is_empty() {
            println!("   Homepage:    {}", m.homepage.join(" "));
        }

        println!("   Description: {}", m.description);

        if let Some(ref pm) = pkg_meta {
            if let Some(ref ld) = pm.longdescription {
                for line in wrap(ld, 72) {
                    println!("                {line}");
                }
            }
        }

        if let Some(ref lic) = m.license {
            println!("   License:     {lic}");
        }

        println!("   Slot:        {}", m.slot);

        if !m.keywords.is_empty() {
            let kws: Vec<String> = m.keywords.iter().map(|k| k.to_string()).collect();
            println!("   Keywords:    {}", kws.join(" "));
        }

        if let Some(vdb) = vdb {
            let installed = find_packages(vdb, &cpv.cpn.to_string());
            if !installed.is_empty() {
                println!("   Installed:");
                for pkg in &installed {
                    println!("     Version:   {}", pkg.cpv().version);
                    if let Ok(slot) = pkg.slot() {
                        println!("     Slot:      {slot}");
                    }
                    if let Ok(repo_name) = pkg.repository() {
                        if let Some(r) = repo_name {
                            println!("     Repo:      {r}");
                        }
                    }
                    if let Ok(Some(ts)) = pkg.build_time() {
                        let t = UNIX_EPOCH + Duration::from_secs(ts);
                        println!("     Built:     {}", humantime::format_rfc3339_seconds(t));
                    }
                    if let Ok(Some(bytes)) = pkg.size() {
                        println!("     Size:      {}", format_size(bytes, BINARY));
                    }
                    if let Ok(flags) = pkg.use_flags() {
                        if !flags.is_empty() {
                            println!("     USE:       {}", flags.join(" "));
                        }
                    }
                }
            }
        }

        println!();
    }
    Ok(())
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current.clone());
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}
