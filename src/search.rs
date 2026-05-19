use std::collections::BTreeSet;
use std::path::Path;

use portage_repo::Repository;

use crate::error::{Error, Result};

pub fn run(repo_path: &Path, pattern: &str, search_description: bool) -> Result<()> {
    let repo = Repository::open(repo_path).map_err(|e| Error::Other(e.to_string()))?;

    let mut seen: BTreeSet<String> = BTreeSet::new();

    for ebuild in repo.ebuilds().map_err(|e| Error::Other(e.to_string()))? {
        let cpv = ebuild.cpv();
        let cpn = cpv.cpn.to_string();

        if seen.contains(&cpn) {
            continue;
        }

        let name_match = cpn.contains(pattern);

        // Read cache once; needed for description display and optional desc search
        let desc = if name_match || search_description {
            repo.cache_entry(cpv)
                .ok()
                .map(|e| e.metadata.description.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };

        let matched = name_match || (search_description && desc.contains(pattern));

        if matched {
            seen.insert(cpn.clone());
            println!("{cpn}: {desc}");
        }
    }
    Ok(())
}
