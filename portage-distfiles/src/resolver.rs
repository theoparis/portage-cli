use std::collections::{HashMap, HashSet};

use portage_metadata::SrcUriEntry;
use portage_repo::Repository;

use crate::error::{Error, Result};

/// A fully resolved distfile: local filename + all candidate download URLs.
///
/// URLs are in priority order — try each in turn until one succeeds.
#[derive(Debug, Clone)]
pub struct Distfile {
    /// The local filename to store in DISTDIR.
    pub filename: String,
    /// Download URLs in priority order (mirrors expanded, GENTOO_MIRRORS appended).
    pub urls: Vec<String>,
    /// EAPI 8+ restriction: `Some("fetch")` or `Some("mirror")`.
    pub restriction: Option<String>,
}

/// Resolves `SRC_URI` entries into concrete [`Distfile`]s.
///
/// Expands `mirror://` URIs using the repository's `thirdpartymirrors` data
/// and appends GENTOO_MIRRORS as a fallback for every distfile.
pub struct DistfileResolver {
    /// Parsed `profiles/thirdpartymirrors`: mirror name → list of base URLs.
    thirdparty: HashMap<String, Vec<String>>,
    /// GENTOO_MIRRORS — appended as final fallback for every distfile.
    gentoo_mirrors: Vec<String>,
}

impl DistfileResolver {
    /// Build a resolver from explicit data (useful for testing).
    pub fn new(thirdparty: Vec<(String, Vec<String>)>, gentoo_mirrors: Vec<String>) -> Self {
        Self {
            thirdparty: thirdparty.into_iter().collect(),
            gentoo_mirrors,
        }
    }

    /// Build a resolver from a live repository + a GENTOO_MIRRORS list.
    ///
    /// `gentoo_mirrors` should come from the `GENTOO_MIRRORS` environment
    /// variable or `make.conf`, split on whitespace.
    pub fn from_repo(repo: &Repository, gentoo_mirrors: Vec<String>) -> Result<Self> {
        let thirdparty = repo
            .thirdpartymirrors()
            .map_err(|e| Error::Manifest(e.to_string()))?;
        Ok(Self::new(thirdparty, gentoo_mirrors))
    }

    /// Resolve `SRC_URI` entries into distfiles given the active USE flags.
    ///
    /// USE-conditional groups are evaluated; `mirror://` URIs are expanded.
    /// GENTOO_MIRRORS are appended as final fallback for every distfile.
    pub fn resolve(&self, entries: &[SrcUriEntry], use_flags: &HashSet<String>) -> Vec<Distfile> {
        let mut raw: Vec<(String, String, Option<String>)> = Vec::new();
        collect_uri_pairs(entries, use_flags, &mut raw);

        raw.into_iter()
            .map(|(url, filename, restriction)| {
                let urls = self.expand_url(&url, &filename);
                Distfile { filename, urls, restriction }
            })
            .collect()
    }

    /// Expand a single URL to one or more concrete download URLs.
    ///
    /// `mirror://name/path` is expanded via `thirdpartymirrors`. Every URL
    /// also has GENTOO_MIRRORS appended as a last-resort fallback.
    fn expand_url(&self, url: &str, filename: &str) -> Vec<String> {
        let mut urls = Vec::new();

        if let Some(rest) = url.strip_prefix("mirror://") {
            // mirror://name/path → look up name in thirdpartymirrors.
            let (mirror_name, path) = rest.split_once('/').unwrap_or((rest, ""));
            if let Some(bases) = self.thirdparty.get(mirror_name) {
                for base in bases {
                    let base = base.trim_end_matches('/');
                    urls.push(format!("{base}/{path}"));
                }
            }
            // If the mirror name is unknown, fall through to GENTOO_MIRRORS only.
        } else {
            urls.push(url.to_owned());
        }

        // Append GENTOO_MIRRORS as final fallback: ${mirror}/distfiles/${filename}.
        for mirror in &self.gentoo_mirrors {
            let mirror = mirror.trim_end_matches('/');
            urls.push(format!("{mirror}/distfiles/{filename}"));
        }

        urls
    }
}

/// Walk `SRC_URI` entries collecting `(url, filename, restriction)` tuples.
///
/// USE-conditional groups are evaluated against `use_flags`.
/// This is the public equivalent of the private `collect_src_filenames`
/// in `portage-repo`.
pub fn collect_filenames(entries: &[SrcUriEntry], use_flags: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    collect_filenames_inner(entries, use_flags, &mut out);
    out
}

fn collect_filenames_inner(
    entries: &[SrcUriEntry],
    use_flags: &HashSet<String>,
    out: &mut Vec<String>,
) {
    for entry in entries {
        match entry {
            SrcUriEntry::Uri { filename, .. } => out.push(filename.clone()),
            SrcUriEntry::Renamed { target, .. } => out.push(target.clone()),
            SrcUriEntry::UseConditional { flag, negated, entries } => {
                let active = use_flags.contains(flag.as_str());
                if active != *negated {
                    collect_filenames_inner(entries, use_flags, out);
                }
            }
            SrcUriEntry::Group(entries) => {
                collect_filenames_inner(entries, use_flags, out);
            }
        }
    }
}

fn collect_uri_pairs(
    entries: &[SrcUriEntry],
    use_flags: &HashSet<String>,
    out: &mut Vec<(String, String, Option<String>)>,
) {
    for entry in entries {
        match entry {
            SrcUriEntry::Uri { url, filename, restriction } => {
                out.push((url.clone(), filename.clone(), restriction.clone()));
            }
            SrcUriEntry::Renamed { url, target, restriction } => {
                out.push((url.clone(), target.clone(), restriction.clone()));
            }
            SrcUriEntry::UseConditional { flag, negated, entries } => {
                let active = use_flags.contains(flag.as_str());
                if active != *negated {
                    collect_uri_pairs(entries, use_flags, out);
                }
            }
            SrcUriEntry::Group(entries) => {
                collect_uri_pairs(entries, use_flags, out);
            }
        }
    }
}
