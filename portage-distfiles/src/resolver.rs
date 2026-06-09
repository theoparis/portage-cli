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
    /// Resolve `SRC_URI` entries into distfiles given the active USE flags.
    ///
    /// USE-conditional groups are evaluated; `mirror://` URIs are expanded.
    /// GENTOO_MIRRORS are appended as a fallback for every distfile that is
    /// not `mirror`-restricted.
    pub fn resolve(&self, entries: &[SrcUriEntry], use_flags: &HashSet<String>) -> Vec<Distfile> {
        let mut raw: Vec<(String, String, Option<String>)> = Vec::new();
        collect_uri_pairs(entries, use_flags, &mut raw);

        raw.into_iter()
            .map(|(url, filename, restriction)| {
                let mut urls = self.expand_url(&url, &filename);
                // Append GENTOO_MIRRORS as a final fallback, but only when:
                // - not mirror-restricted (EAPI 8 `mirror+` prefix), AND
                // - not mirror://gentoo/ (expand_url already resolved those to
                //   GENTOO_MIRRORS with the correct full path suffix).
                if restriction.as_deref() != Some("mirror") && !url.starts_with("mirror://gentoo/")
                {
                    for mirror in &self.gentoo_mirrors {
                        let mirror = mirror.trim_end_matches('/');
                        let fallback = format!("{mirror}/distfiles/{filename}");
                        if !urls.contains(&fallback) {
                            urls.push(fallback);
                        }
                    }
                }
                Distfile {
                    filename,
                    urls,
                    restriction,
                }
            })
            .collect()
    }

    /// Expand a single URL to one or more concrete download URLs.
    ///
    /// `mirror://gentoo/path` uses GENTOO_MIRRORS with the full path suffix.
    /// `mirror://name/path` (other names) is expanded via `thirdpartymirrors`.
    /// Direct URLs are returned as-is; GENTOO_MIRRORS fallback is added by
    /// the caller (`resolve`) so it can be gated on the restriction flag.
    fn expand_url(&self, url: &str, filename: &str) -> Vec<String> {
        if let Some(rest) = url.strip_prefix("mirror://") {
            let (mirror_name, path) = rest.split_once('/').unwrap_or((rest, filename));
            if mirror_name == "gentoo" {
                // mirror://gentoo/path → GENTOO_MIRRORS with the full path suffix.
                // Using the path (not just filename) preserves any subdirectory.
                return self
                    .gentoo_mirrors
                    .iter()
                    .map(|m| format!("{}/distfiles/{path}", m.trim_end_matches('/')))
                    .collect();
            }
            if let Some(bases) = self.thirdparty.get(mirror_name) {
                return bases
                    .iter()
                    .map(|base| format!("{}/{path}", base.trim_end_matches('/')))
                    .collect();
            }
            // Unknown mirror name — no direct URLs; caller will add GENTOO_MIRRORS
            // as a last-resort fallback (unless mirror-restricted).
            vec![]
        } else {
            vec![url.to_owned()]
        }
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
            SrcUriEntry::UseConditional {
                flag,
                negated,
                entries,
            } => {
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
            SrcUriEntry::Uri {
                url,
                filename,
                restriction,
            } => {
                out.push((url.clone(), filename.clone(), restriction.clone()));
            }
            SrcUriEntry::Renamed {
                url,
                target,
                restriction,
            } => {
                out.push((url.clone(), target.clone(), restriction.clone()));
            }
            SrcUriEntry::UseConditional {
                flag,
                negated,
                entries,
            } => {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(gentoo_mirrors: &[&str]) -> DistfileResolver {
        DistfileResolver::new(
            vec![(
                "kde".to_owned(),
                vec!["https://mirrors.kde.org/".to_owned()],
            )],
            gentoo_mirrors.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn direct_url_gets_gentoo_fallback() {
        let r = resolver(&["https://mirror.gentoo.org"]);
        let entries = SrcUriEntry::parse("https://example.com/foo-1.0.tar.gz").unwrap();
        let dfs = r.resolve(&entries, &HashSet::new());
        assert_eq!(
            dfs[0].urls,
            [
                "https://example.com/foo-1.0.tar.gz",
                "https://mirror.gentoo.org/distfiles/foo-1.0.tar.gz",
            ]
        );
    }

    #[test]
    fn mirror_gentoo_uses_full_path() {
        let r = resolver(&["https://mirror.gentoo.org"]);
        let entries = SrcUriEntry::parse("mirror://gentoo/subdir/foo-1.0.tar.gz").unwrap();
        let dfs = r.resolve(&entries, &HashSet::new());
        // Full path preserved, not just filename.
        assert_eq!(
            dfs[0].urls,
            ["https://mirror.gentoo.org/distfiles/subdir/foo-1.0.tar.gz"]
        );
        assert_eq!(dfs[0].urls.len(), 1);
    }

    #[test]
    fn mirror_restriction_suppresses_gentoo_fallback() {
        let r = resolver(&["https://mirror.gentoo.org"]);
        let entries = vec![SrcUriEntry::Renamed {
            url: "https://proprietary.example.com/secret.tar.gz".to_owned(),
            target: "secret.tar.gz".to_owned(),
            restriction: Some("mirror".to_owned()),
        }];
        let dfs = r.resolve(&entries, &HashSet::new());
        assert_eq!(
            dfs[0].urls,
            ["https://proprietary.example.com/secret.tar.gz"]
        );
        assert_eq!(dfs[0].urls.len(), 1);
    }

    #[test]
    fn thirdparty_mirror_expansion() {
        let r = resolver(&[]);
        let entries = SrcUriEntry::parse("mirror://kde/stable/frameworks/foo.tar.xz").unwrap();
        let dfs = r.resolve(&entries, &HashSet::new());
        assert_eq!(
            dfs[0].urls,
            ["https://mirrors.kde.org/stable/frameworks/foo.tar.xz"]
        );
    }
}
