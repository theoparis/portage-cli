//! Local binary-package reuse (`-k`/`--usepkg`): decide, for a resolved plan
//! entry, whether a GPKG in `PKGDIR` is reusable as-is or must be rebuilt.
//!
//! A binpkg is reusable only when it would produce the same result as a fresh
//! build — portage's rule: the binpkg's recorded `USE`, restricted to the
//! package's own `IUSE`, must equal the desired `USE` (similarly restricted),
//! and the slot/subslot must match. Version matches by `cpv` lookup. This is
//! the [`_match_use`]-style check portage applies to built packages
//! (`use = USE ∩ IUSE`), so a stale-USE binpkg is correctly rejected and
//! rebuilt — matching `emerge -k`.
//!
//! The fast path reads the `Packages` index `em maint binhost` writes (one
//! `KEY: VALUE` block per package); if it is absent the index is rebuilt on the
//! fly by scanning `PKGDIR` and reading each container's metadata.
//!
//! [`_match_use`]: https://github.com/gentoo/portage/blob/ac461a29/lib/portage/dbapi/__init__.py

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use portage_repo::MakeConf;

use crate::cli::Cli;

/// Real portage's own hardcoded system default — for the host-root test
/// only; `resolve_pkgdir` no longer references this directly since
/// `merge_root.join("var/cache/binpkgs")` reduces to the same string when
/// `merge_root` is `/`.
#[cfg(test)]
const DEFAULT_PKGDIR: &str = "/var/cache/binpkgs";
const MAKE_GLOBALS: &str = "/usr/share/portage/config/make.globals";

/// Resolve `PKGDIR`: `$PKGDIR` env → `make.conf` (config root) → `make.globals`
/// → `/var/cache/binpkgs`. Shared by `em maint binhost` and the `-k` consumer.
///
/// The `make.globals`/hardcoded-default steps are **host** defaults — real
/// portage's own system-wide install convention, unconditionally
/// `/var/cache/binpkgs` (confirmed: this repo's own `make.globals` hardcodes
/// exactly that). For a `--root`/`--target`/`--local`/`--prefix` build (any
/// merge root other than `/`), consulting that host default is wrong: it's a
/// real, root-owned system path the build has no business writing to, and
/// unprivileged builds can't anyway. Caught live: a stage3 `--buildpkg` run
/// tried to write there, got `EACCES`, and appears to have destabilized the
/// fakeroost ptrace session for several packages — see
/// `todo/stage-build-shakeout.md`. Skip straight to a root-relative default
/// in that case; `$PKGDIR`/config-root `make.conf` (explicit user choices)
/// still apply regardless of root.
pub(crate) fn resolve_pkgdir(globals: &Cli) -> Utf8PathBuf {
    if let Ok(v) = std::env::var("PKGDIR")
        && !v.trim().is_empty()
    {
        return Utf8PathBuf::from(v);
    }
    if let Some(v) = read_make_conf_var(globals, "PKGDIR")
        && !v.is_empty()
    {
        return Utf8PathBuf::from(v);
    }
    let merge_root = globals.roots().merge_root().to_owned();
    // make.globals is a host-level default; only consult it for a real host
    // build. A non-host root falls through to the join below unconditionally
    // — no separate "is this the host?" branch needed there, since
    // `"/".join("var/cache/binpkgs")` already *is* the host default.
    if merge_root.as_str() == "/" {
        let mg = Utf8Path::new(MAKE_GLOBALS);
        if mg.exists()
            && let Ok(mc) = MakeConf::load(mg)
            && let Some(v) = mc.get("PKGDIR").filter(|s| !s.is_empty())
        {
            return Utf8PathBuf::from(v);
        }
    }
    merge_root.join("var/cache/binpkgs")
}

/// Read a variable from `make.conf` under the resolved config root.
pub(crate) fn read_make_conf_var(globals: &Cli, var: &str) -> Option<String> {
    let cfg_root = globals
        .roots()
        .config()
        .map(|c| c.to_path_buf())
        .unwrap_or_else(|| Utf8PathBuf::from("/"));
    for rel in ["etc/portage/make.conf", "etc/make.conf"] {
        let p = cfg_root.join(rel);
        if p.exists()
            && let Ok(mc) = MakeConf::load(&p)
            && let Some(v) = mc.get(var).filter(|s| !s.is_empty())
        {
            return Some(v.to_owned());
        }
    }
    None
}

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
            let text = std::fs::read_to_string(&index_path)
                .with_context(|| format!("reading {}", index_path.display()))?;
            match Self::parse(&text, pkgdir.to_path_buf()) {
                Ok(idx) if !idx.entries.is_empty() => return Ok(idx),
                _ => {}
            }
        }
        Self::scan(pkgdir)
    }

    /// Parse a `Packages` file. The first blank-line-separated block is the
    /// header; each later block is one package (`CPV:` required).
    fn parse(text: &str, pkgdir: PathBuf) -> Result<Self> {
        let entries = parse_packages_entries(text);
        Ok(Self { entries, pkgdir })
    }

    /// Slow path: no usable index — scan `pkgdir` and read each container's
    /// metadata via `portage_binpkg::read_metadata`.
    fn scan(pkgdir: &Path) -> Result<Self> {
        let mut entries = BTreeMap::new();
        let mut files = Vec::new();
        find_gpkgs(pkgdir, pkgdir, &mut files)?;
        for (rel, full) in &files {
            let meta = match portage_binpkg::read_metadata(full) {
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
fn split_use(s: &str) -> HashSet<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

/// Split an `IUSE` string, stripping `+`/`-` default-on/off prefixes so the
/// flag names compare against `USE`.
fn split_iuse(s: &str) -> HashSet<String> {
    s.split_whitespace()
        .map(|t| t.trim_start_matches(['+', '-']).to_owned())
        .collect()
}

/// Parse a `Packages` index into `cpv → entry`. Shared by the local and remote
/// consumers (the only difference is how `path` is resolved: a local `PKGDIR`
/// join vs a remote `base_uri` join). The header block (no `CPV:`) is skipped.
pub fn parse_packages_entries(text: &str) -> BTreeMap<String, BinpkgEntry> {
    let mut entries = BTreeMap::new();
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
        // override) is not yet honoured — tracked in PENDING.
        Some(format!("{}/{path}", self.base_uri, path = entry.path))
    }
}

/// One `binrepos.conf` section — real portage's `BinRepoConfig`, restricted
/// to the fields em's remote binpkg fetch path uses. `frozen`/
/// `verify_signature` are parsed and carried but not yet *enforced*: `frozen`
/// ("prefer a locally cached index over fetching fresh") needs the
/// not-yet-built local index cache to have any effect, and
/// `verify_signature` needs the not-yet-built GPG verify step — both already
/// tracked in `todo/PENDING.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinRepoEntry {
    /// Section name, or an md5 hex digest of the `sync-uri` for a
    /// `PORTAGE_BINHOST`-derived implicit entry (matches real portage's own
    /// `_digest_uri` naming — this is display/debugging only, never a sort
    /// tie-breaker in practice: implicit entries always get a distinct
    /// priority `>= 1`, so they never actually tie against an explicit
    /// section's default `priority` of `0`).
    pub name: String,
    /// The binhost base URI, trailing slash stripped.
    pub sync_uri: String,
    pub frozen: bool,
    pub verify_signature: bool,
}

/// Resolve the configured remote binhosts: `binrepos.conf` (global defaults,
/// then `${PORTAGE_CONFIGROOT}/etc/portage/binrepos.conf` — either may be a
/// directory of `*.conf` files, real portage's own two-path search order,
/// `dbapi/bintree.py`'s `getbinpkgs` `config_paths`) plus legacy
/// `PORTAGE_BINHOST`, combined in real portage's own priority order
/// (`BinRepoConfigLoader.__init__`): explicit sections use their own
/// `priority =` (default `0`, ties broken by name); `PORTAGE_BINHOST`'s
/// space-separated URLs are folded in as unnamed, auto-prioritized entries,
/// skipping any URL an explicit section already covers. The combined list is
/// sorted **ascending** by `(priority, name)` and then **reversed** for
/// final order — matching `bintree.py`'s own
/// `reversed(list(self._binrepos_conf.values()))`. For a plain
/// `PORTAGE_BINHOST` list with no `binrepos.conf` at all, the two reversals
/// cancel out, netting the original left-to-right order (verified against
/// real portage's source, not assumed — see the unit tests below). Used by
/// `-g`/`--getbinpkg`.
///
/// Simplification vs real portage's `ConfigParser`: no `%(VAR)s`
/// interpolation, and a `[DEFAULT]` section's keys are not inherited into
/// other sections (same simplification `ReposConf` already makes for
/// `repos.conf`'s own `[DEFAULT]`/`main-repo`) — no configured value
/// observed in practice needs either.
pub(crate) fn portage_binhosts(globals: &Cli) -> Vec<BinRepoEntry> {
    let config_root = globals
        .roots()
        .config()
        .map(|c| c.to_path_buf())
        .unwrap_or_else(|| Utf8PathBuf::from("/"));

    let mut sections: std::collections::HashMap<String, std::collections::HashMap<String, String>> =
        std::collections::HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for rel in [
        "usr/share/portage/config/binrepos.conf",
        "etc/portage/binrepos.conf",
    ] {
        let path = config_root.join(rel);
        for file in portage_repo::ini::collect_conf_files(path.as_std_path()).unwrap_or_default() {
            if let Ok(contents) = std::fs::read_to_string(&file) {
                portage_repo::ini::merge_sections(&mut sections, &mut order, &contents);
            }
        }
    }

    let binhost_var = std::env::var("PORTAGE_BINHOST")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| read_make_conf_var(globals, "PORTAGE_BINHOST").filter(|v| !v.is_empty()));

    combine_binhosts(&sections, &order, binhost_var.as_deref())
}

/// The pure core of [`portage_binhosts`]: combine parsed `binrepos.conf`
/// sections with a legacy `PORTAGE_BINHOST` value into the final,
/// priority-ordered list. Split out from the I/O (file reads, env var,
/// `make.conf`) so the priority/reversal algorithm — the part most worth
/// getting exactly right — is unit-testable without mutating the real
/// process environment (`PORTAGE_BINHOST` is process-global; tests run
/// threaded within one process, so setting it in a test would race any
/// other test touching the same var).
fn combine_binhosts(
    sections: &std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    order: &[String],
    binhost_var: Option<&str>,
) -> Vec<BinRepoEntry> {
    let mut seen_uris: HashSet<String> = HashSet::new();
    // (priority, name) carried alongside each entry purely for the final
    // sort — not part of the public `BinRepoEntry`, since callers only ever
    // need the already-resolved order.
    let mut repos: Vec<(Option<i64>, String, BinRepoEntry)> = Vec::new();
    for name in order {
        let Some(s) = sections.get(name) else {
            continue;
        };
        let Some(sync_uri) = s.get("sync-uri").map(|v| normalize_binhost_uri(v)) else {
            eprintln!("warning: missing sync-uri setting for binrepo {name}");
            continue;
        };
        seen_uris.insert(sync_uri.clone());
        let priority = s.get("priority").and_then(|v| v.parse::<i64>().ok());
        repos.push((
            priority,
            name.clone(),
            BinRepoEntry {
                name: name.clone(),
                sync_uri,
                frozen: parse_binrepo_bool(s.get("frozen")),
                verify_signature: parse_binrepo_bool(s.get("verify-signature")),
            },
        ));
    }

    if let Some(val) = binhost_var {
        let mut current_priority: i64 = 0;
        for url in val.split_whitespace().rev() {
            let sync_uri = normalize_binhost_uri(url);
            if seen_uris.insert(sync_uri.clone()) {
                current_priority += 1;
                let name = format!("{:x}", md5::compute(sync_uri.as_bytes()));
                repos.push((
                    Some(current_priority),
                    name.clone(),
                    BinRepoEntry {
                        name,
                        sync_uri,
                        frozen: false,
                        verify_signature: false,
                    },
                ));
            }
        }
    }

    repos.sort_by(|a, b| (a.0.unwrap_or(0), &a.1).cmp(&(b.0.unwrap_or(0), &b.1)));
    repos.into_iter().rev().map(|(_, _, e)| e).collect()
}

fn normalize_binhost_uri(uri: &str) -> String {
    uri.trim().trim_end_matches('/').to_string()
}

fn parse_binrepo_bool(v: Option<&String>) -> bool {
    matches!(v.map(|s| s.to_lowercase()), Some(s) if s == "true" || s == "yes")
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

/// Recursively enumerate `*.gpkg.tar` files under `root` as `(rel, full)`.
fn find_gpkgs(dir: &Path, root: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            find_gpkgs(&entry.path(), root, out)?;
        } else if ft.is_file() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".gpkg.tar") {
                let full = entry.path();
                let rel = full
                    .strip_prefix(root)
                    .with_context(|| "stripping PKGDIR prefix")?
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, full));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Regression test for the stage3 --buildpkg failure: a non-host root
    /// must never default PKGDIR to the real system's `/var/cache/binpkgs`
    /// (root-owned, not writable, and not even meaningful for a different
    /// root's package cache) — see `resolve_pkgdir`'s doc comment.
    #[test]
    fn non_host_root_gets_root_relative_pkgdir_default() {
        assert!(
            std::env::var("PKGDIR").is_err(),
            "test assumes no ambient PKGDIR override"
        );
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let cli = Cli::parse_from(["em", "--root", root]);
        let pkgdir = resolve_pkgdir(&cli);
        assert_eq!(
            pkgdir,
            camino::Utf8Path::new(root).join("var/cache/binpkgs")
        );
    }

    /// A plain host build (root `/`, no --root/--prefix/--local/--target) is
    /// unaffected by the root-aware branch — it still falls through to the
    /// pre-existing make.globals/hardcoded-default lookup, exactly as before
    /// this change.
    #[test]
    fn host_root_skips_the_root_relative_branch() {
        assert!(
            std::env::var("PKGDIR").is_err(),
            "test assumes no ambient PKGDIR override"
        );
        // `["em"]` alone (zero args) trips clap's `arg_required_else_help`
        // (prints help and exits the process) — pass --root explicitly.
        let cli = Cli::parse_from(["em", "--root", "/"]);
        assert_eq!(cli.roots().merge_root().as_str(), "/");
        let expected = {
            let mg = Utf8Path::new(MAKE_GLOBALS);
            if mg.exists()
                && let Ok(mc) = MakeConf::load(mg)
                && let Some(v) = mc.get("PKGDIR").filter(|s| !s.is_empty())
            {
                Utf8PathBuf::from(v)
            } else {
                Utf8PathBuf::from(DEFAULT_PKGDIR)
            }
        };
        assert_eq!(resolve_pkgdir(&cli), expected);
    }

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
        let idx = BinpkgIndex::parse(text, PathBuf::from("/pkgdir")).unwrap();
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

    fn parse_sections(
        contents: &str,
    ) -> (
        std::collections::HashMap<String, std::collections::HashMap<String, String>>,
        Vec<String>,
    ) {
        let mut sections = std::collections::HashMap::new();
        let mut order = Vec::new();
        portage_repo::ini::merge_sections(&mut sections, &mut order, contents);
        (sections, order)
    }

    fn uris(entries: &[BinRepoEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.sync_uri.as_str()).collect()
    }

    /// The two reversals in real portage's own algorithm (`BinRepoConfigLoader`
    /// assigns increasing priority walking `PORTAGE_BINHOST` *backwards*;
    /// `bintree.py` then consumes the whole sorted list *reversed*) cancel out
    /// for a plain `PORTAGE_BINHOST` with no `binrepos.conf` at all — verified
    /// against the real source, not assumed (see `binrepo/config.py` +
    /// `dbapi/bintree.py`).
    #[test]
    fn plain_portage_binhost_preserves_original_order() {
        let (sections, order) = parse_sections("");
        let result = combine_binhosts(&sections, &order, Some("A B C"));
        assert_eq!(uris(&result), vec!["A", "B", "C"]);
    }

    /// A higher `priority =` in `binrepos.conf` is tried *first* (ascending
    /// sort, then reversed for consumption — a higher number sorts later
    /// ascending, so ends up first after the reversal).
    #[test]
    fn binrepos_conf_priority_higher_number_tried_first() {
        let (sections, order) = parse_sections(
            "[low]\nsync-uri = http://low\npriority = 1\n\n\
             [high]\nsync-uri = http://high\npriority = 10\n",
        );
        let result = combine_binhosts(&sections, &order, None);
        assert_eq!(uris(&result), vec!["http://high", "http://low"]);
    }

    /// Explicit `binrepos.conf` sections (priority defaults to 0) and legacy
    /// `PORTAGE_BINHOST` entries (always priority >= 1) combine correctly:
    /// the `PORTAGE_BINHOST` entries outrank the unprioritized section.
    #[test]
    fn binrepos_conf_and_portage_binhost_combine() {
        let (sections, order) = parse_sections("[mine]\nsync-uri = http://mine\n");
        let result = combine_binhosts(&sections, &order, Some("http://a http://b"));
        // http://a and http://b (priority 2 and 1 respectively, per the
        // reversed-walk rule) outrank the unprioritized (priority 0) `mine`.
        assert_eq!(uris(&result), vec!["http://a", "http://b", "http://mine"]);
    }

    /// A `PORTAGE_BINHOST` URL already covered by an explicit `binrepos.conf`
    /// section is not duplicated.
    #[test]
    fn duplicate_sync_uri_is_not_added_twice() {
        let (sections, order) = parse_sections("[mine]\nsync-uri = http://dup\npriority = 5\n");
        let result = combine_binhosts(&sections, &order, Some("http://dup http://new"));
        assert_eq!(result.len(), 2);
        assert_eq!(uris(&result), vec!["http://dup", "http://new"]);
    }

    /// A section with no `sync-uri` is skipped entirely (matching real
    /// portage's own warn-and-skip behaviour), not merged with a blank URI.
    #[test]
    fn missing_sync_uri_is_skipped() {
        let (sections, order) = parse_sections("[broken]\npriority = 1\n");
        let result = combine_binhosts(&sections, &order, None);
        assert!(result.is_empty());
    }

    #[test]
    fn frozen_and_verify_signature_parsed_case_insensitively() {
        let (sections, order) =
            parse_sections("[a]\nsync-uri = http://a\nfrozen = True\nverify-signature = yes\n");
        let result = combine_binhosts(&sections, &order, None);
        assert_eq!(result.len(), 1);
        assert!(result[0].frozen);
        assert!(result[0].verify_signature);
    }

    #[test]
    fn frozen_and_verify_signature_default_false() {
        let (sections, order) = parse_sections("[a]\nsync-uri = http://a\n");
        let result = combine_binhosts(&sections, &order, None);
        assert_eq!(result.len(), 1);
        assert!(!result[0].frozen);
        assert!(!result[0].verify_signature);
    }

    /// Exercises the real `portage_binhosts` entry point end-to-end against a
    /// real file on disk (not just `combine_binhosts`'s pure core): a real
    /// `--root`, a real `etc/portage/binrepos.conf` file, real
    /// `collect_conf_files`/`merge_sections` I/O.
    #[test]
    fn portage_binhosts_reads_a_real_binrepos_conf_file() {
        assert!(
            std::env::var("PORTAGE_BINHOST").is_err(),
            "test assumes no ambient PORTAGE_BINHOST override"
        );
        let dir = tempfile::tempdir().unwrap();
        let portage_dir = dir.path().join("etc/portage");
        std::fs::create_dir_all(&portage_dir).unwrap();
        std::fs::write(
            portage_dir.join("binrepos.conf"),
            "[myhost]\nsync-uri = https://example.invalid/binhost\npriority = 3\n",
        )
        .unwrap();

        // `config()` defaults to the real host `/` for a bare `--root`
        // (portage `ROOT=`/`PORTAGE_CONFIGROOT` parity — see
        // `base_roots()`'s doc comment); `--config-root` is required here so
        // this test reads only the tempdir's own file, never the real host's
        // `/etc/portage/binrepos.conf`.
        let root = dir.path().to_str().unwrap();
        let cli = Cli::parse_from(["em", "--root", root, "--config-root", root]);
        let result = portage_binhosts(&cli);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "myhost");
        assert_eq!(result[0].sync_uri, "https://example.invalid/binhost");
    }
}
