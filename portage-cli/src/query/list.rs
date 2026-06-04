use std::path::Path;

use anyhow::Result;
use portage_repo::Repository;
use portage_vdb::Vdb;

pub fn run(repo_path: &Path, patterns: &[String]) -> Result<()> {
    let repo = Repository::open(repo_path)?;

    let mut cpvs: Vec<String> = repo
        .ebuilds()?
        .into_iter()
        .map(|ebuild| ebuild.cpv().to_string())
        .filter(|cpv| patterns.is_empty() || patterns.iter().any(|p| matches_pattern(cpv, p)))
        .collect();

    cpvs.sort();
    for cpv in &cpvs {
        println!("{cpv}");
    }
    Ok(())
}

pub fn run_installed(vdb: &Vdb, patterns: &[String]) -> Result<()> {
    let mut cpvs: Vec<String> = vdb
        .packages()
        .into_iter()
        .map(|pkg| pkg.to_string())
        .filter(|cpv| patterns.is_empty() || patterns.iter().any(|p| matches_pattern(cpv, p)))
        .collect();

    cpvs.sort();
    for cpv in &cpvs {
        println!("{cpv}");
    }
    Ok(())
}

fn matches_pattern(s: &str, pattern: &str) -> bool {
    if pattern.contains('*') {
        glob_match(s.as_bytes(), pattern.as_bytes())
    } else {
        s.contains(pattern)
    }
}

fn glob_match(s: &[u8], p: &[u8]) -> bool {
    match (s.first(), p.first()) {
        (_, Some(b'*')) => glob_match(s, &p[1..]) || (!s.is_empty() && glob_match(&s[1..], p)),
        (Some(sc), Some(pc)) if sc == pc => glob_match(&s[1..], &p[1..]),
        (None, None) => true,
        _ => false,
    }
}
