//! The binhost `Packages` index format: parsing, and the local/remote
//! reuse-matching readers `em`'s `-k`/`--usepkg`/`-g`/`--getbinpkg` build on.
//!
//! A binpkg is reusable only when it would produce the same result as a
//! fresh build — portage's rule: the binpkg's recorded `USE`, restricted to
//! the package's own `IUSE`, must equal the desired `USE` (similarly
//! restricted), and the slot/subslot must match. Version matches by `cpv`
//! lookup. This is the `_match_use`-style check portage applies to built
//! packages (`use = USE ∩ IUSE`), so a stale-USE binpkg is correctly
//! rejected and rebuilt — matching `emerge -k`
//! (<https://github.com/gentoo/portage/blob/ac461a29/lib/portage/dbapi/__init__.py>,
//! bug #453400).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::scan::find_gpkg_containers;

/// One `Packages` index entry, parsed into the fields the reuse check needs.
#[derive(Debug, Clone)]
pub struct BinpkgEntry {
    /// Path relative to `PKGDIR` (e.g. `app-test/foo-1.0-1.gpkg.tar`).
    pub path: String,
    /// The binpkg's recorded `USE`, as a bare-flag set.
    pub use_set: HashSet<String>,
    /// The package's `IUSE`, prefix-stripped (`+flag`/`-flag` → `flag`).
    pub iuse: HashSet<String>,
}

/// A parsed `Packages` index, keyed by `cpv`, answering reuse queries.
#[derive(Debug, Default)]
pub struct BinpkgIndex {
    entries: BTreeMap<String, BinpkgEntry>,
    /// Absolute `PKGDIR`, used to resolve each entry's relative `path`.
    pkgdir: PathBuf,
}

impl BinpkgIndex {
    /// Open the `Packages` index in `pkgdir`. If it is missing or unreadable,
    /// rebuild it on the fly by scanning `pkgdir` for `*.gpkg.tar` and reading
    /// each container's metadata (the slow fallback).
    pub fn open(pkgdir: &Path) -> Result<Self> {
        let index_path = pkgdir.join("Packages");
        if index_path.is_file() {
            let text = std::fs::read_to_string(&index_path)?;
            let idx = Self::parse(&text, pkgdir.to_path_buf());
            if !idx.entries.is_empty() {
                return Ok(idx);
            }
        }
        Self::scan(pkgdir)
    }

    /// Parse a `Packages` file. The first blank-line-separated block is the
    /// header; each later block is one package (`CPV:` required).
    fn parse(text: &str, pkgdir: PathBuf) -> Self {
        let entries = parse_packages_entries(text);
        Self { entries, pkgdir }
    }

    /// Slow path: no usable index — scan `pkgdir` and read each container's
    /// metadata via [`crate::read_metadata`].
    fn scan(pkgdir: &Path) -> Result<Self> {
        let mut entries = BTreeMap::new();
        let mut files = Vec::new();
        find_gpkg_containers(pkgdir, pkgdir, &mut files)?;
        for (rel, full) in &files {
            let meta = match crate::read_metadata(full) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("warning: skipping {}: {e:#}", full.display());
                    continue;
                }
            };
            let cat = meta.get("CATEGORY").map(String::as_str).unwrap_or("");
            let pf = meta.get("PF").map(String::as_str).unwrap_or("");
            if cat.is_empty() || pf.is_empty() {
                continue;
            }
            let cpv = format!("{cat}/{pf}");
            entries.insert(
                cpv.clone(),
                BinpkgEntry {
                    path: rel.clone(),
                    use_set: split_use(meta.get("USE").map(String::as_str).unwrap_or("")),
                    iuse: split_iuse(meta.get("IUSE").map(String::as_str).unwrap_or("")),
                },
            );
        }
        Ok(Self {
            entries,
            pkgdir: pkgdir.to_path_buf(),
        })
    }

    /// The number of indexed packages (for reporting).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the index has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Find a reusable binpkg for `cpv` given the desired `USE`, returning the
    /// absolute container path. `None` if no binpkg exists for the cpv, or if
    /// its recorded USE does not match (i.e. it must be rebuilt). Version and
    /// slot match by `cpv` lookup (a binpkg for a cpv is that ebuild's slot).
    pub fn find_reusable(&self, cpv: &str, desired_use: &[String]) -> Option<PathBuf> {
        let entry = self.entries.get(cpv)?;
        if !use_compatible(&entry.use_set, &entry.iuse, desired_use) {
            return None;
        }
        Some(self.pkgdir.join(&entry.path))
    }
}

/// Split a `USE` string into a bare-flag set.
pub(crate) fn split_use(s: &str) -> HashSet<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

/// Split an `IUSE` string, stripping `+`/`-` default-on/off prefixes so the
/// flag names compare against `USE`.
pub(crate) fn split_iuse(s: &str) -> HashSet<String> {
    s.split_whitespace()
        .map(|t| t.trim_start_matches(['+', '-']).to_owned())
        .collect()
}

/// Split a `Packages` index into its per-package `KEY: VALUE` blocks (the
/// header block, which has no `CPV:` line, is skipped). Shared by every
/// consumer that needs a different subset of fields than [`BinpkgEntry`]
/// carries — e.g. `em maint binpkg`'s verify/list/prune, which also need
/// `MD5`/`SHA1`/`SIZE`/`BUILD_ID`.
pub fn parse_index_blocks(text: &str) -> Vec<BTreeMap<&str, &str>> {
    let mut blocks = Vec::new();
    for block in text.split("\n\n") {
        let block = block.trim();
        if block.is_empty() || !block.lines().any(|l| l.starts_with("CPV:")) {
            continue;
        }
        let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
        for line in block.lines() {
            if let Some((k, v)) = line.split_once(": ") {
                fields.insert(k, v);
            }
        }
        if fields.contains_key("CPV") {
            blocks.push(fields);
        }
    }
    blocks
}

/// Parse a `Packages` index into `cpv → entry`. Shared by the local and remote
/// consumers (the only difference is how `path` is resolved: a local `PKGDIR`
/// join vs a remote `base_uri` join).
pub fn parse_packages_entries(text: &str) -> BTreeMap<String, BinpkgEntry> {
    let mut entries = BTreeMap::new();
    for fields in parse_index_blocks(text) {
        let Some(&cpv) = fields.get("CPV") else {
            continue;
        };
        entries.insert(
            cpv.to_string(),
            BinpkgEntry {
                path: fields.get("PATH").copied().unwrap_or("").to_string(),
                use_set: split_use(fields.get("USE").copied().unwrap_or("")),
                iuse: split_iuse(fields.get("IUSE").copied().unwrap_or("")),
            },
        );
    }
    entries
}

/// A remote binhost's `Packages` index, parsed from a fetched index text and a
/// base URI. Mirrors [`BinpkgIndex`] but resolves each entry's `PATH` to a
/// download URL instead of a local file — used by `-g`/`--getbinpkg`.
#[derive(Debug, Clone)]
pub struct RemoteBinpkgIndex {
    entries: BTreeMap<String, BinpkgEntry>,
    base_uri: String,
}

impl RemoteBinpkgIndex {
    /// Build from a fetched index body and the binhost base URI (the `sync-uri`
    /// / `PORTAGE_BINHOST` entry the index was fetched from).
    pub fn new(index_text: &str, base_uri: &str) -> Self {
        Self {
            entries: parse_packages_entries(index_text),
            base_uri: base_uri.trim_end_matches('/').to_string(),
        }
    }

    /// The number of indexed packages (for reporting).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the index has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Find a reusable remote binpkg for `cpv`, returning its download URL.
    /// `None` if the cpv is absent or its USE does not match the desired set
    /// (same `use_compatible` rule as the local index).
    pub fn find_reusable(&self, cpv: &str, desired_use: &[String]) -> Option<String> {
        let entry = self.entries.get(cpv)?;
        if !use_compatible(&entry.use_set, &entry.iuse, desired_use) {
            return None;
        }
        // portage: download URL = BASE_URI + "/" + PATH. PATH is the per-entry
        // index field; the binhost's own `URI` header (a server-controlled
        // override) is not yet honoured — tracked in `em`'s PENDING.md.
        Some(format!("{}/{path}", self.base_uri, path = entry.path))
    }
}

/// The reuse core: is a binpkg's `USE` (restricted to its `IUSE`) equal to the
/// desired `USE` (restricted to `IUSE`)? Flags outside `IUSE` (USE_EXPAND
/// defaults, profile-implicit flags) don't affect the package and are ignored.
/// This is portage's built-package USE check (bug #453400).
pub fn use_compatible(
    binpkg_use: &HashSet<String>,
    iuse: &HashSet<String>,
    desired_use: &[String],
) -> bool {
    for flag in iuse {
        let in_binpkg = binpkg_use.contains(flag);
        let in_desired = desired_use.iter().any(|d| d == flag);
        if in_binpkg != in_desired {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn desired(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn identical_use_is_reusable() {
        // binpkg built with nls,debug; IUSE nls,debug,ssl; desired nls,debug.
        assert!(use_compatible(
            &set(&["nls", "debug"]),
            &set(&["nls", "debug", "ssl"]),
            &desired(&["nls", "debug"]),
        ));
    }

    #[test]
    fn differing_iuse_flag_is_stale() {
        // binpkg has nls on, desired has it off (within IUSE) → not reusable.
        assert!(!use_compatible(
            &set(&["nls", "debug"]),
            &set(&["nls", "debug", "ssl"]),
            &desired(&["debug"]),
        ));
    }

    #[test]
    fn non_iuse_flags_are_ignored() {
        // A USE_EXPAND/implicit flag not in IUSE (python_targets_3_13) differs,
        // but since it's not in IUSE it must not block reuse.
        assert!(use_compatible(
            &set(&["nls", "python_targets_3_13"]),
            &set(&["nls"]),
            &desired(&["nls"]),
        ));
        // And conversely.
        assert!(use_compatible(
            &set(&["nls"]),
            &set(&["nls"]),
            &desired(&["nls", "python_targets_3_13"]),
        ));
    }

    #[test]
    fn iuse_default_prefixes_are_stripped() {
        // IUSE="+ssl -debug" → flags ssl,debug. A binpkg with ssl off and desired
        // ssl on (within IUSE) is stale.
        let iuse = split_iuse("+ssl -debug nls");
        assert_eq!(iuse, set(&["ssl", "debug", "nls"]));
        assert!(!use_compatible(
            &set(&["nls"]),
            &iuse,
            &desired(&["nls", "ssl"])
        ));
    }

    #[test]
    fn ssl_off_in_both_is_reusable() {
        // ssl is in IUSE but off in both binpkg and desired → reusable.
        assert!(use_compatible(
            &set(&["nls"]),
            &set(&["nls", "ssl"]),
            &desired(&["nls"]),
        ));
    }

    #[test]
    fn parses_packages_index_blocks() {
        let text = "\
CHOST: aarch64-unknown-linux-gnu
VERSION: 0
PACKAGES: 2
TIMESTAMP: 1700000000

BUILD_ID: 1
CPV: app-test/foo-1.0
IUSE: +nls -debug
PATH: app-test/foo-1.0-1.gpkg.tar
SLOT: 0/5.1
USE: nls

CPV: app-test/bar-2.0
IUSE: ssl
PATH: app-test/bar-2.0-1.gpkg.tar
SLOT: 0
USE: ssl
";
        let idx = BinpkgIndex::parse(text, PathBuf::from("/pkgdir"));
        assert_eq!(idx.len(), 2);

        // foo: nls on matches desired [nls]; debug off in both → reusable.
        let p = idx
            .find_reusable("app-test/foo-1.0", &desired(&["nls"]))
            .unwrap();
        assert_eq!(p, PathBuf::from("/pkgdir/app-test/foo-1.0-1.gpkg.tar"));

        // foo with nls off → stale (nls is in IUSE, differs) → None.
        assert!(
            idx.find_reusable("app-test/foo-1.0", &desired(&[]))
                .is_none()
        );

        // bar: ssl matches → reusable.
        assert!(
            idx.find_reusable("app-test/bar-2.0", &desired(&["ssl"]))
                .is_some()
        );

        // Wrong cpv → None.
        assert!(
            idx.find_reusable("app-test/missing-9.9", &desired(&["nls"]))
                .is_none()
        );
    }

    #[test]
    fn remote_index_resolves_to_download_url() {
        // Same index text as the local case, but resolved against a binhost
        // base URI → find_reusable returns a URL, not a local path.
        let text = "\
VERSION: 0
PACKAGES: 1

CPV: app-test/foo-1.0
IUSE: +nls -debug
PATH: app-test/foo-1.0-1.gpkg.tar
USE: nls
";
        let idx = RemoteBinpkgIndex::new(text, "https://binhost.example/");
        assert_eq!(idx.len(), 1);
        // Trailing slash on base_uri is trimmed; URL = base + "/" + PATH.
        assert_eq!(
            idx.find_reusable("app-test/foo-1.0", &desired(&["nls"]))
                .unwrap(),
            "https://binhost.example/app-test/foo-1.0-1.gpkg.tar"
        );
        // Stale USE → None (same use_compatible rule as local).
        assert!(
            idx.find_reusable("app-test/foo-1.0", &desired(&["nls", "debug"]))
                .is_none()
        );
        // Unknown cpv → None.
        assert!(
            idx.find_reusable("app-test/missing-9.9", &desired(&["nls"]))
                .is_none()
        );
    }
}
