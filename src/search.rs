use std::collections::BTreeSet;
use std::path::Path;

use portage_repo::Repository;

use crate::error::{Error, Result};

pub fn run(repo_path: &Path, pattern: &str, search_description: bool) -> Result<()> {
    let repo = Repository::open(repo_path).map_err(|e| Error::Other(e.to_string()))?;

    let ebuilds = repo
        .ebuilds()
        .map_err(|e| Error::Other(e.to_string()))?;

    // Collect unique CPNs that match; if --description, also check DESCRIPTION from cache
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for ebuild in ebuilds {
        let cpv = ebuild.cpv();
        let cpn = cpv.cpn.to_string();

        if seen.contains(&cpn) {
            continue;
        }

        let name_match = cpn.contains(pattern);

        let desc_match = if search_description && !name_match {
            repo.cache_entry(cpv)
                .ok()
                .map(|e| e.metadata.description.contains(pattern))
                .unwrap_or(false)
        } else {
            false
        };

        if name_match || desc_match {
            seen.insert(cpn.clone());
            if search_description {
                let desc = repo
                    .cache_entry(cpv)
                    .ok()
                    .map(|e| e.metadata.description.clone())
                    .unwrap_or_default();
                println!("{cpn}");
                if !desc.is_empty() {
                    println!("    {desc}");
                }
            } else {
                println!("{cpn}");
            }
        }
    }
    Ok(())
}
