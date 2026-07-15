//! `em maint binpkg` — local `PKGDIR` maintenance on top of the `Packages`
//! index/reader substrate (`binhost.rs`, `crate::binpkg`).
//!
//! No real-portage `emaint` module covers this ground (its own `emaint
//! binhost` only regenerates the index) — this is an em-only extension,
//! built to a scope this repo can actually verify:
//!
//! - `verify`: for each indexed container, recompute size/MD5/SHA1 and
//!   compare against the index's recorded values — the same check
//!   `_emerge/BinpkgVerifier.py` performs before reusing a binpkg (size
//!   first, then digests), just run in bulk instead of per-merge.
//! - `list`: a plain table over the index (cpv, build-id, size, path).
//! - `prune`: em keeps at most one container per cpv in its own reuse model
//!   (see the `binpkg-multi-instance` gap in `todo/PENDING.md`) — this
//!   collapses any leftover extra `BUILD_ID`s down to the newest one and
//!   regenerates the index, rather than reimplementing gentoolkit's
//!   `eclean-pkg` (which also prunes by installed-set/age/size and isn't
//!   available on this host to verify against).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use camino::Utf8Path;
use humansize::{BINARY, format_size};

use crate::binpkg::{parse_index_blocks, read_make_conf_var, resolve_pkgdir};
use crate::cli::{BinpkgAction, Cli};

use super::binhost::{checksum, find_gpkg_containers, index_pkgdir, parse_build_id_from_name};

/// Dispatch `em maint binpkg <action>`.
pub fn run(action: &BinpkgAction, globals: &Cli) -> Result<()> {
    let pkgdir = resolve_pkgdir(globals);
    let chost = || read_make_conf_var(globals, "CHOST").unwrap_or_default();
    match action {
        BinpkgAction::Verify { fix } => verify(&pkgdir, &chost(), *fix),
        BinpkgAction::List => list(&pkgdir),
        BinpkgAction::Prune { dry_run } => prune(&pkgdir, &chost(), *dry_run),
    }
}

/// One `Packages` index entry, with the digest/size fields `verify`/`list`
/// need that [`crate::binpkg::BinpkgEntry`] doesn't carry.
struct IndexRow {
    cpv: String,
    path: String,
    md5: Option<String>,
    sha1: Option<String>,
    size: Option<u64>,
    build_id: Option<u32>,
}

fn parse_index_rows(text: &str) -> Vec<IndexRow> {
    parse_index_blocks(text)
        .into_iter()
        .map(|fields| IndexRow {
            cpv: fields.get("CPV").copied().unwrap_or("").to_string(),
            path: fields.get("PATH").copied().unwrap_or("").to_string(),
            md5: fields.get("MD5").map(|s| s.to_string()),
            sha1: fields.get("SHA1").map(|s| s.to_string()),
            size: fields.get("SIZE").and_then(|s| s.parse().ok()),
            build_id: fields.get("BUILD_ID").and_then(|s| s.parse().ok()),
        })
        .collect()
}

fn read_index(pkgdir: &Utf8Path) -> Result<Vec<IndexRow>> {
    let index_path = pkgdir.join("Packages");
    if !index_path.is_file() {
        bail!("no Packages index at {index_path} — run `em maint binhost` first");
    }
    let text = std::fs::read_to_string(index_path.as_std_path())
        .with_context(|| format!("reading {index_path}"))?;
    Ok(parse_index_rows(&text))
}

/// Check each indexed container's size/MD5/SHA1 against the file on disk.
/// `fix`: quarantine corrupt containers (rename to `.corrupt`) and
/// regenerate the index afterward, so missing/corrupt entries are dropped.
fn verify(pkgdir: &Utf8Path, chost: &str, fix: bool) -> Result<()> {
    let rows = read_index(pkgdir)?;

    let mut ok = 0usize;
    let mut missing = 0usize;
    let mut corrupt = 0usize;

    for row in &rows {
        if row.path.is_empty() {
            continue;
        }
        let full = pkgdir.join(&row.path);
        if !full.exists() {
            println!("!!! missing: {} ({})", row.cpv, row.path);
            missing += 1;
            continue;
        }

        let (actual_md5, actual_sha1, actual_size, _mtime) =
            checksum(full.as_std_path()).with_context(|| format!("checksumming {full}"))?;

        let size_ok = row.size.is_none_or(|s| s == actual_size);
        let md5_ok = row.md5.as_deref().is_none_or(|m| m == actual_md5);
        let sha1_ok = row.sha1.as_deref().is_none_or(|s| s == actual_sha1);

        if size_ok && md5_ok && sha1_ok {
            ok += 1;
            continue;
        }

        corrupt += 1;
        println!("!!! digest mismatch: {} ({})", row.cpv, row.path);
        if !size_ok {
            println!(
                "    size: got {actual_size}, expected {}",
                row.size.unwrap()
            );
        }
        if !md5_ok {
            println!(
                "    MD5: got {actual_md5}, expected {}",
                row.md5.as_deref().unwrap()
            );
        }
        if !sha1_ok {
            println!(
                "    SHA1: got {actual_sha1}, expected {}",
                row.sha1.as_deref().unwrap()
            );
        }
        if fix {
            let quarantined = Utf8Path::new(&format!("{full}.corrupt")).to_owned();
            std::fs::rename(full.as_std_path(), quarantined.as_std_path())
                .with_context(|| format!("quarantining {full}"))?;
            println!("    quarantined to {quarantined}");
        }
    }

    println!(
        "emaint binpkg verify: {ok} ok, {corrupt} corrupt, {missing} missing (of {})",
        rows.len()
    );

    if fix && (corrupt > 0 || missing > 0) {
        let (count, _skipped) = index_pkgdir(pkgdir, chost)?;
        println!("emaint binpkg verify: reindexed -> {count} package(s)");
    }

    if !fix && (corrupt > 0 || missing > 0) {
        bail!("{corrupt} corrupt, {missing} missing binpkg(s) found (rerun with --fix)");
    }
    Ok(())
}

/// List indexed binary packages: cpv, build-id, human-readable size, path.
fn list(pkgdir: &Utf8Path) -> Result<()> {
    let rows = read_index(pkgdir)?;

    for row in &rows {
        let size = row
            .size
            .map(|s| format_size(s, BINARY))
            .unwrap_or_else(|| "?".to_string());
        let build_id = row.build_id.map(|b| b.to_string()).unwrap_or_default();
        println!("{:<45} {build_id:>4}  {size:>10}  {}", row.cpv, row.path);
    }
    println!("{} package(s) in {pkgdir}", rows.len());
    Ok(())
}

/// Keep only the newest `BUILD_ID` container per cpv, delete the rest, and
/// regenerate the index. `dry_run`: report what would be deleted without
/// touching anything.
fn prune(pkgdir: &Utf8Path, chost: &str, dry_run: bool) -> Result<()> {
    if !pkgdir.exists() {
        bail!("PKGDIR does not exist: {pkgdir}");
    }

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    find_gpkg_containers(pkgdir.as_std_path(), pkgdir.as_std_path(), &mut files)?;

    // Group container files by cpv, each carrying its resolved build-id.
    let mut by_cpv: BTreeMap<String, Vec<(u32, String, PathBuf)>> = BTreeMap::new();
    for (rel, full) in &files {
        let Some(cpv) = container_cpv(full) else {
            continue;
        };
        let build_id = container_build_id(full, rel);
        by_cpv
            .entry(cpv)
            .or_default()
            .push((build_id, rel.clone(), full.clone()));
    }

    let mut removed = 0usize;
    for (cpv, mut variants) in by_cpv {
        if variants.len() < 2 {
            continue;
        }
        variants.sort_by_key(|(build_id, ..)| *build_id);
        let (kept_id, kept_rel, _) = variants.last().unwrap();
        println!("{cpv}: keeping build {kept_id} ({kept_rel})");
        for (build_id, rel, full) in &variants[..variants.len() - 1] {
            if dry_run {
                println!("{cpv}: would remove build {build_id} ({rel})");
            } else {
                std::fs::remove_file(full)
                    .with_context(|| format!("removing {}", full.display()))?;
                println!("{cpv}: removed build {build_id} ({rel})");
            }
            removed += 1;
        }
    }

    if removed == 0 {
        println!(
            "emaint binpkg prune: nothing to prune ({} package(s))",
            files.len()
        );
        return Ok(());
    }

    if dry_run {
        println!(
            "emaint binpkg prune: {removed} old build(s) would be removed (dry run, index untouched)"
        );
        return Ok(());
    }

    let (count, _skipped) = index_pkgdir(pkgdir, chost)?;
    println!(
        "emaint binpkg prune: removed {removed} old build(s), reindexed -> {count} package(s)"
    );
    Ok(())
}

/// `category/PF` for a container, read from its own metadata.
fn container_cpv(full: &Path) -> Option<String> {
    let meta = portage_binpkg::read_metadata(full).ok()?;
    let cat = meta.get("CATEGORY")?;
    let pf = meta.get("PF")?;
    if cat.is_empty() || pf.is_empty() {
        return None;
    }
    Some(format!("{cat}/{pf}"))
}

/// A container's `BUILD_ID`: prefer the metadata's own field, else parse it
/// from the `<PF>-<BUILD_ID>.gpkg.tar` filename, else `0` (the implicit
/// single-instance case — sorts below any explicit build id, so it's always
/// pruned in favor of a numbered one sharing the same cpv).
fn container_build_id(full: &Path, rel: &str) -> u32 {
    portage_binpkg::read_metadata(full)
        .ok()
        .and_then(|meta| meta.get("BUILD_ID")?.parse().ok())
        .or_else(|| parse_build_id_from_name(rel))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_binpkg::{GpkgInput, write_gpkg};

    /// Seed `pkgdir/<cat>/<pf>-<build_id>.gpkg.tar`, a real container with
    /// enough metadata for `container_cpv`/`container_build_id` and for
    /// `em maint binhost`'s own indexer to pick it up.
    fn seed_container(work: &Path, pkgdir: &Utf8Path, cat: &str, pf: &str, build_id: u32) {
        let image = work.join(format!("image-{pf}-{build_id}"));
        std::fs::create_dir_all(image.join("usr/bin")).unwrap();
        std::fs::write(image.join("usr/bin/hello"), b"hi\n").unwrap();

        let meta = work.join(format!("vdb-{pf}-{build_id}"));
        std::fs::create_dir_all(&meta).unwrap();
        for (k, v) in [
            ("PF", pf),
            ("CATEGORY", cat),
            ("SLOT", "0"),
            ("EAPI", "8"),
            ("repository", "gentoo"),
            ("BUILD_ID", &build_id.to_string()),
        ] {
            std::fs::write(meta.join(k), format!("{v}\n")).unwrap();
        }

        let container = pkgdir.join(format!("{cat}/{pf}-{build_id}.gpkg.tar"));
        std::fs::create_dir_all(container.parent().unwrap()).unwrap();
        write_gpkg(
            &GpkgInput {
                image_dir: &image,
                metadata_dir: &meta,
                basename: pf,
            },
            container.as_std_path(),
        )
        .unwrap();
    }

    #[test]
    fn verify_reports_ok_for_an_untouched_container() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        index_pkgdir(pkgdir, "x86_64-pc-linux-gnu").unwrap();

        verify(pkgdir, "x86_64-pc-linux-gnu", false).unwrap();
    }

    #[test]
    fn verify_without_fix_errors_on_a_corrupted_container() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        index_pkgdir(pkgdir, "x86_64-pc-linux-gnu").unwrap();

        let container = pkgdir.join("app-test/foo-1.0-1.gpkg.tar");
        let mut bytes = std::fs::read(&container).unwrap();
        bytes.push(0xff);
        std::fs::write(&container, bytes).unwrap();

        assert!(verify(pkgdir, "x86_64-pc-linux-gnu", false).is_err());
        assert!(container.exists(), "not quarantined without --fix");
    }

    #[test]
    fn verify_with_fix_quarantines_and_reindexes() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        index_pkgdir(pkgdir, "x86_64-pc-linux-gnu").unwrap();

        let container = pkgdir.join("app-test/foo-1.0-1.gpkg.tar");
        let mut bytes = std::fs::read(&container).unwrap();
        bytes.push(0xff);
        std::fs::write(&container, bytes).unwrap();

        verify(pkgdir, "x86_64-pc-linux-gnu", true).unwrap();

        assert!(!container.exists());
        assert!(pkgdir.join("app-test/foo-1.0-1.gpkg.tar.corrupt").exists());
        let idx = std::fs::read_to_string(pkgdir.join("Packages")).unwrap();
        assert!(!idx.contains("CPV: app-test/foo-1.0"));
    }

    #[test]
    fn verify_reports_a_missing_container() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        index_pkgdir(pkgdir, "x86_64-pc-linux-gnu").unwrap();
        std::fs::remove_file(pkgdir.join("app-test/foo-1.0-1.gpkg.tar")).unwrap();

        assert!(verify(pkgdir, "x86_64-pc-linux-gnu", false).is_err());
    }

    #[test]
    fn prune_keeps_only_the_newest_build_id() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 2);

        prune(pkgdir, "x86_64-pc-linux-gnu", false).unwrap();

        assert!(!pkgdir.join("app-test/foo-1.0-1.gpkg.tar").exists());
        assert!(pkgdir.join("app-test/foo-1.0-2.gpkg.tar").exists());
        let idx = std::fs::read_to_string(pkgdir.join("Packages")).unwrap();
        assert!(idx.contains("PATH: app-test/foo-1.0-2.gpkg.tar"));
    }

    #[test]
    fn prune_dry_run_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 2);

        prune(pkgdir, "x86_64-pc-linux-gnu", true).unwrap();

        assert!(pkgdir.join("app-test/foo-1.0-1.gpkg.tar").exists());
        assert!(pkgdir.join("app-test/foo-1.0-2.gpkg.tar").exists());
    }

    #[test]
    fn list_prints_without_error_over_a_real_index() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgdir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        seed_container(tmp.path(), pkgdir, "app-test", "foo-1.0", 1);
        index_pkgdir(pkgdir, "x86_64-pc-linux-gnu").unwrap();

        list(pkgdir).unwrap();
    }

    #[test]
    fn parse_index_rows_reads_digest_and_size_fields() {
        let text = "VERSION: 0\n\nCPV: app-test/foo-1.0\nPATH: app-test/foo-1.0-1.gpkg.tar\nMD5: abc\nSHA1: def\nSIZE: 42\nBUILD_ID: 1\n\n";
        let rows = parse_index_rows(text);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cpv, "app-test/foo-1.0");
        assert_eq!(rows[0].path, "app-test/foo-1.0-1.gpkg.tar");
        assert_eq!(rows[0].md5.as_deref(), Some("abc"));
        assert_eq!(rows[0].sha1.as_deref(), Some("def"));
        assert_eq!(rows[0].size, Some(42));
        assert_eq!(rows[0].build_id, Some(1));
    }

    #[test]
    fn parse_index_rows_skips_the_header_block() {
        let text =
            "VERSION: 0\nPACKAGES: 1\n\nCPV: app-test/foo-1.0\nPATH: app-test/foo-1.0.gpkg.tar\n\n";
        let rows = parse_index_rows(text);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].build_id.is_none());
    }
}
