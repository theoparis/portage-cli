use std::io::Write as _;
use std::path::Path;

use anyhow::Result;
use portage_atom::Cpv;
use portage_repo::Repository;
use portage_vdb::Vdb;

use crate::cli::{C_CAT, C_PKGNAME, C_VERSION};

pub fn run(repo_path: &Path, patterns: &[String]) -> Result<()> {
    let repo = Repository::open(repo_path)?;

    let mut cpvs: Vec<Cpv> = repo
        .ebuilds()?
        .into_iter()
        .map(|ebuild| ebuild.cpv().clone())
        .filter(|cpv| {
            patterns.is_empty()
                || patterns
                    .iter()
                    .any(|p| matches_pattern(&cpv.to_string(), p))
        })
        .collect();

    cpvs.sort();
    let mut out = anstream::stdout();
    for cpv in &cpvs {
        writeln!(
            out,
            "{C_CAT}{}{C_CAT:#}/{C_PKGNAME}{}{C_PKGNAME:#}-{C_VERSION}{}{C_VERSION:#}",
            cpv.cpn.category, cpv.cpn.package, cpv.version
        )
        .ok();
    }
    Ok(())
}

pub fn run_installed(vdb: &Vdb, patterns: &[String]) -> Result<()> {
    let mut cpvs: Vec<Cpv> = vdb
        .packages()
        .into_iter()
        .map(|pkg| pkg.cpv().clone())
        .filter(|cpv| {
            patterns.is_empty()
                || patterns
                    .iter()
                    .any(|p| matches_pattern(&cpv.to_string(), p))
        })
        .collect();

    cpvs.sort();
    let mut out = anstream::stdout();
    for cpv in &cpvs {
        writeln!(
            out,
            "{C_CAT}{}{C_CAT:#}/{C_PKGNAME}{}{C_PKGNAME:#}-{C_VERSION}{}{C_VERSION:#}",
            cpv.cpn.category, cpv.cpn.package, cpv.version
        )
        .ok();
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
