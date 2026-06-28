//! `em maint binhost` — regenerate the binpkg `Packages` index.
//!
//! Walks `PKGDIR` for `*.gpkg.tar`, reads each container's VDB-style metadata
//! via [`portage_binpkg::read_metadata`], and writes the `Packages` index
//! portage/emerge consume over `PORTAGE_BINHOST` / `--getbinpkg`. The format is
//! portage's `binarytree._populate_local` / `PackageIndex.write`:
//!
//! - a **header** block (sorted `KEY: VALUE` lines, blank-line separated), then
//! - one block per package (sorted by CPV, `KEY: VALUE` for non-empty values,
//!   blank line between entries).
//!
//! Per-package entry = the gpkg's metadata (with `DESCRIPTION`→`DESC` and
//! `repository`→`REPO` translations) plus the container file's `MD5`/`SHA1`
//! checksums, `SIZE`, `MTIME`, relative `PATH`, `CPV` and `BUILD_ID`.
//!
//! Header profile-derived keys (`USE`, `USE_EXPAND`, `ACCEPT_KEYWORDS`, …) and
//! USE-pre-evaluated dep strings are intentionally minimal here: portage
//! evaluates dep conditionals on read using each entry's `USE`, and a sparse
//! header is valid (its `_pkgindex_version_supported` only requires `VERSION`).
//! `REPO_REVISIONS` (the repo git-revision-at-build) is omitted — em does not
//! yet track sync history at build time.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use camino::Utf8Path;
use sha1::Digest as _;
use sha1::Sha1;

use crate::binpkg::{read_make_conf_var, resolve_pkgdir};
use crate::cli::Cli;

/// Dispatch `em maint binhost`.
pub fn run(globals: &Cli) -> Result<()> {
    let pkgdir = resolve_pkgdir(globals);
    if !pkgdir.exists() {
        bail!("PKGDIR does not exist: {}", pkgdir);
    }
    let chost = read_make_conf_var(globals, "CHOST").unwrap_or_default();
    let (count, skipped) = index_pkgdir(&pkgdir, &chost)?;
    println!(
        "emaint binhost: indexed {} package(s){} -> {}/Packages",
        count,
        if skipped > 0 {
            format!(", skipped {skipped}")
        } else {
            String::new()
        },
        pkgdir
    );
    Ok(())
}

/// Walk `pkgdir` for `*.gpkg.tar`, build per-package entries, and write the
/// `Packages` index. Returns `(indexed, skipped)`.
fn index_pkgdir(pkgdir: &Utf8Path, chost: &str) -> Result<(usize, usize)> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    find_gpkg_containers(pkgdir.as_std_path(), pkgdir.as_std_path(), &mut files)?;
    files.sort();

    let mut header = BTreeMap::new();
    header.insert("VERSION".to_string(), "0".to_string());
    header.insert("CHOST".to_string(), chost.to_string());
    header.insert("repository".to_string(), String::new());

    let mut entries: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    let mut skipped = 0usize;
    for (rel, full) in &files {
        match build_entry(pkgdir.as_std_path(), rel, full) {
            Ok((cpv, fields)) => entries.push((cpv, fields)),
            Err(e) => {
                eprintln!("warning: skipping {}: {e:#}", full.display());
                skipped += 1;
            }
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    write_index(pkgdir, &header, &entries)?;
    Ok((entries.len(), skipped))
}

/// Recursively enumerate `*.gpkg.tar` container files under `root`, as
/// `(rel_path, full)` pairs.
fn find_gpkg_containers(dir: &Path, root: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            find_gpkg_containers(&entry.path(), root, out)?;
        } else if ft.is_file() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".gpkg.tar") {
                let full = entry.path();
                let rel = full
                    .strip_prefix(root)
                    .with_context(|| format!("stripping PKGDIR prefix from {}", full.display()))?
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, full));
            }
        }
    }
    Ok(())
}

/// Build one index entry from a container: metadata fields + file checksums.
///
/// Returns `(cpv, fields)` where `fields` keys are already in their
/// index-translated form (`DESC`, `REPO`).
fn build_entry(
    _pkgdir: &Path,
    rel: &str,
    full: &Path,
) -> Result<(String, BTreeMap<String, String>)> {
    let meta = portage_binpkg::read_metadata(full)
        .with_context(|| format!("reading metadata from {}", full.display()))?;

    let cat = meta.get("CATEGORY").map(String::as_str).unwrap_or("");
    let pf = meta.get("PF").map(String::as_str).unwrap_or("");
    if cat.is_empty() || pf.is_empty() {
        bail!("missing CATEGORY/PF in metadata");
    }
    let cpv = format!("{cat}/{pf}");

    let (md5, sha1, size, mtime) = checksum(full)?;

    let mut f = BTreeMap::new();
    f.insert("CPV".to_string(), cpv.clone());
    f.insert("MD5".to_string(), md5);
    f.insert("SHA1".to_string(), sha1);
    f.insert("SIZE".to_string(), size.to_string());
    f.insert("MTIME".to_string(), mtime.to_string());
    f.insert("PATH".to_string(), rel.to_string());

    if let Some(bid) = meta.get("BUILD_ID") {
        f.insert("BUILD_ID".to_string(), bid.clone());
    } else if let Some(bid) = parse_build_id_from_name(rel) {
        f.insert("BUILD_ID".to_string(), bid.to_string());
    }

    // Identity metadata fields (skip empty); the two portage translations are
    // applied so the file matches portage's own index.
    copy_field(&meta, &mut f, "BUILD_TIME");
    copy_field(&meta, &mut f, "SLOT");
    copy_field(&meta, &mut f, "EAPI");
    copy_field(&meta, &mut f, "USE");
    copy_field(&meta, &mut f, "IUSE");
    copy_field(&meta, &mut f, "KEYWORDS");
    copy_field(&meta, &mut f, "LICENSE");
    copy_field(&meta, &mut f, "RESTRICT");
    copy_field(&meta, &mut f, "PROPERTIES");
    copy_field(&meta, &mut f, "DEFINED_PHASES");
    copy_field(&meta, &mut f, "DEPEND");
    copy_field(&meta, &mut f, "RDEPEND");
    copy_field(&meta, &mut f, "BDEPEND");
    copy_field(&meta, &mut f, "PDEPEND");
    copy_field(&meta, &mut f, "IDEPEND");
    copy_field(&meta, &mut f, "CHOST");
    copy_field(&meta, &mut f, "PROVIDES");
    copy_field(&meta, &mut f, "REQUIRES");

    // Translated fields (metadata name → index name).
    if let Some(v) = meta.get("DESCRIPTION").filter(|s| !s.is_empty()) {
        f.insert("DESC".to_string(), v.clone());
    }
    if let Some(v) = meta.get("repository").filter(|s| !s.is_empty()) {
        f.insert("REPO".to_string(), v.clone());
    }

    Ok((cpv, f))
}

fn copy_field(meta: &BTreeMap<String, String>, out: &mut BTreeMap<String, String>, key: &str) {
    if let Some(v) = meta.get(key).filter(|s| !s.is_empty()) {
        out.insert(key.to_string(), v.clone());
    }
}

/// Container file MD5+SHA1 (lowercase hex), byte size, and mtime (unix secs).
fn checksum(path: &Path) -> Result<(String, String, u64, u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut md5 = md5::Context::new();
    let mut sha1 = Sha1::new();
    let mut buf = [0u8; 65536];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        md5.write_all(&buf[..n])?;
        sha1.update(&buf[..n]);
        size += n as u64;
    }
    let md5 = format!("{:x}", md5.compute());
    let sha1 = hex::encode(sha1.finalize());

    let mtime = std::fs::metadata(path)?
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    Ok((md5, sha1, size, mtime))
}

/// Parse the trailing `-<n>` build-id from a container basename, portage's
/// `<PF>-<BUILD_ID>.gpkg.tar` layout. `None` for the single-instance
/// `<PF>.gpkg.tar` form.
fn parse_build_id_from_name(rel: &str) -> Option<u32> {
    let base = rel.rsplit('/').next()?;
    let stem = base.strip_suffix(".gpkg.tar")?;
    let (rest, id) = stem.rsplit_once('-')?;
    if rest.is_empty() {
        return None;
    }
    id.parse::<u32>().ok()
}

/// Write the `Packages` index: header block, then one block per entry.
fn write_index(
    pkgdir: &Utf8Path,
    header: &BTreeMap<String, String>,
    entries: &[(String, BTreeMap<String, String>)],
) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut text = String::new();
    for (k, v) in header {
        if v.is_empty() {
            continue;
        }
        text.push_str(k);
        text.push_str(": ");
        text.push_str(v);
        text.push('\n');
    }
    text.push_str(&format!("PACKAGES: {}\n", entries.len()));
    text.push_str(&format!("TIMESTAMP: {now}\n"));
    text.push('\n');

    for (_cpv, fields) in entries {
        for (k, v) in fields {
            if v.is_empty() {
                continue;
            }
            text.push_str(k);
            text.push_str(": ");
            text.push_str(v);
            text.push('\n');
        }
        text.push('\n');
    }

    let out = pkgdir.join("Packages");
    std::fs::write(&out, text).with_context(|| format!("writing {out}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_binpkg::write_gpkg;

    /// Seed a PKGDIR with a gpkg via the writer, then build the index and
    /// verify the `Packages` file has the expected header + one sorted entry
    /// with checksums and the translated DESC/REPO keys.
    #[test]
    fn index_roundtrips_a_written_gpkg() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();

        let image = tmp.path().join("image");
        std::fs::create_dir_all(image.join("usr/bin")).unwrap();
        std::fs::write(image.join("usr/bin/hello"), b"hi\n").unwrap();

        let meta = tmp.path().join("vdb/foo-1.0");
        std::fs::create_dir_all(&meta).unwrap();
        for (k, v) in [
            ("PF", "foo-1.0"),
            ("CATEGORY", "app-test"),
            ("SLOT", "0"),
            ("EAPI", "8"),
            ("DESCRIPTION", "a test package"),
            ("repository", "gentoo"),
            ("BUILD_TIME", "1700000000"),
            ("SIZE", "3"),
        ] {
            std::fs::write(meta.join(k), format!("{v}\n")).unwrap();
        }

        let container = pkgdir.join("app-test/foo-1.0-1.gpkg.tar");
        std::fs::create_dir_all(container.parent().unwrap()).unwrap();
        write_gpkg(
            &portage_binpkg::GpkgInput {
                image_dir: &image,
                metadata_dir: &meta,
                basename: "foo-1.0",
            },
            container.as_std_path(),
        )
        .unwrap();

        let (count, skipped) = index_pkgdir(pkgdir, "aarch64-unknown-linux-gnu").unwrap();
        assert_eq!(count, 1);
        assert_eq!(skipped, 0);

        let idx = std::fs::read_to_string(pkgdir.join("Packages")).unwrap();
        assert!(idx.contains("VERSION: 0"));
        assert!(idx.contains("CHOST: aarch64-unknown-linux-gnu"));
        assert!(idx.contains("PACKAGES: 1"));
        // Header sorts before entries; CHOST < VERSION alphabetically, so the
        // file leads with CHOST (portage sorts header keys the same way).
        assert!(idx.starts_with("CHOST: aarch64-unknown-linux-gnu\n"));

        assert!(idx.contains("CPV: app-test/foo-1.0"));
        assert!(idx.contains("PATH: app-test/foo-1.0-1.gpkg.tar"));
        assert!(idx.contains("BUILD_ID: 1"));
        assert!(idx.contains("DESC: a test package"));
        assert!(idx.contains("REPO: gentoo"));
        assert!(idx.contains("MD5: "));
        assert!(idx.contains("SHA1: "));
        assert!(idx.contains("SIZE: "));
        assert!(idx.contains("MTIME: "));
    }
}
