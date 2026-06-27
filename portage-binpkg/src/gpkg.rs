//! GPKG (GLEP 78) binary-package container writer.
//!
//! A GPKG is a **plain (uncompressed) tar** whose members, all owned `0/0`, are —
//! **in this exact order**:
//!
//! 1. `<basename>/gpkg-1` — a 0-byte format marker (must be first),
//! 2. `<basename>/metadata.tar.<c>` — the VDB-style metadata under `metadata/`,
//! 3. `<basename>/image.tar.<c>` — the installed image (`${D}`) under `image/`,
//! 4. `<basename>/Manifest` — `DATA <member> <size> SHA512 .. BLAKE2B ..` per
//!    member (must be last).
//!
//! `<basename>` is the package `PF` (e.g. `gentoo-functions-1.7.6`). The two inner
//! tars are produced with GNU `tar` (`--numeric-owner`, pax `--xattrs` for the
//! image so file capabilities/ACLs and device nodes survive) and compressed with
//! zstd — the Portage default. Requires `tar` and `zstd` on `PATH`.

use std::path::Path;
use std::process::Command;

use blake2::Blake2b512;
use sha2::{Digest, Sha512};

use crate::error::{Error, Result};

/// The GLEP 78 format-marker filename (and version).
const GPKG_VERSION: &str = "gpkg-1";
const METADATA_TAR: &str = "metadata.tar.zst";
const IMAGE_TAR: &str = "image.tar.zst";

/// Inputs for [`write_gpkg`].
pub struct GpkgInput<'a> {
    /// The installed image directory (`${D}`); its contents are packed under
    /// `image/` with ownership/xattrs preserved.
    pub image_dir: &'a Path,
    /// The VDB-style metadata directory (the package's `var/db/pkg/<cat>/<pf>`);
    /// its contents are packed under `metadata/`.
    pub metadata_dir: &'a Path,
    /// The package basename — `PF`, e.g. `gentoo-functions-1.7.6`.
    pub basename: &'a str,
}

/// Build a GPKG from `input` and write it to `out_path`
/// (conventionally `<PKGDIR>/<category>/<PF>-<BUILD_ID>.gpkg.tar`).
///
/// The owner/mode/xattr metadata of `image_dir` is read as it sits on disk, so the
/// caller is responsible for running this where that metadata is correct — inside
/// the privilege session (real root, sudo, or the userns box) for an unprivileged
/// build.
pub fn write_gpkg(input: &GpkgInput, out_path: &Path) -> Result<()> {
    let staging = tempfile::Builder::new().prefix("em-gpkg-").tempdir()?;
    let pkg_dir = staging.path().join(input.basename);
    std::fs::create_dir_all(&pkg_dir)?;

    // 1. the 0-byte format marker.
    let gpkg1 = pkg_dir.join(GPKG_VERSION);
    std::fs::File::create(&gpkg1)?;

    // 2. metadata.tar.zst — the VDB field files under `metadata/`, *flat with no
    //    directory entry*: portage's `get_metadata` does `extractfile(m).read()`
    //    on every member, which is `None` for a dir.
    let metadata = pkg_dir.join(METADATA_TAR);
    tar_metadata(input.metadata_dir, &metadata)?;

    // 3. image.tar.zst — `${D}` under `image/`, with xattrs (caps/ACLs/devnodes).
    let image = pkg_dir.join(IMAGE_TAR);
    tar_tree(input.image_dir, "image", &image, true)?;

    // 4. Manifest — checksums of the three members above (Manifest excludes itself).
    let manifest = pkg_dir.join("Manifest");
    write_manifest(
        &manifest,
        &[
            (GPKG_VERSION, &gpkg1),
            (METADATA_TAR, &metadata),
            (IMAGE_TAR, &image),
        ],
    )?;

    // Container: a plain tar, members added in the required order (gpkg-1 first,
    // Manifest last), forced to 0/0.
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let b = input.basename;
    run(
        "tar",
        Command::new("tar")
            .arg("--numeric-owner")
            .arg("--owner=0")
            .arg("--group=0")
            .arg("--format=ustar")
            .arg("-C")
            .arg(staging.path())
            .arg("-cf")
            .arg(out_path)
            .arg(format!("{b}/{GPKG_VERSION}"))
            .arg(format!("{b}/{METADATA_TAR}"))
            .arg(format!("{b}/{IMAGE_TAR}"))
            .arg(format!("{b}/Manifest")),
    )
}

/// `tar --zstd` the whole *tree* under `dir` into `out`, renaming the root to
/// `prefix` (so members are `prefix/...`, directory entries included). With
/// `xattrs`, file capabilities, ACLs and device nodes are preserved (pax format).
fn tar_tree(dir: &Path, prefix: &str, out: &Path, xattrs: bool) -> Result<()> {
    let mut cmd = Command::new("tar");
    cmd.arg("--zstd")
        .arg("--numeric-owner")
        .arg("--format=pax")
        // Rename the `.`-rooted members to `<prefix>/…`.
        .arg(format!("--transform=s,^\\.,{prefix},"));
    if xattrs {
        cmd.arg("--xattrs").arg("--xattrs-include=*");
    }
    cmd.arg("-C").arg(dir).arg("-cf").arg(out).arg(".");
    run("tar", &mut cmd)
}

/// `tar --zstd` the *files* directly in `dir` (the VDB is flat) into `out`, each
/// member as `metadata/<name>` — with **no** `metadata/` directory entry, since
/// portage reads every metadata member with `extractfile().read()`.
fn tar_metadata(dir: &Path, out: &Path) -> Result<()> {
    let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();
    names.sort();
    let mut cmd = Command::new("tar");
    cmd.arg("--zstd")
        .arg("--numeric-owner")
        .arg("--format=pax")
        .arg("--no-recursion")
        .arg("--transform=s,^,metadata/,")
        .arg("-C")
        .arg(dir)
        .arg("-cf")
        .arg(out);
    cmd.args(&names);
    run("tar", &mut cmd)
}

/// Write a GLEP 74-style Manifest with one `DATA` line per member.
fn write_manifest(out: &Path, members: &[(&str, &std::path::PathBuf)]) -> Result<()> {
    let mut text = String::new();
    for (name, path) in members {
        let data = std::fs::read(path)?;
        let sha512 = hex::encode(Sha512::digest(&data));
        let blake2b = hex::encode(Blake2b512::digest(&data));
        text.push_str(&format!(
            "DATA {name} {} SHA512 {sha512} BLAKE2B {blake2b}\n",
            data.len()
        ));
    }
    std::fs::write(out, text)?;
    Ok(())
}

fn run(tool: &'static str, cmd: &mut Command) -> Result<()> {
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Tool {
            tool,
            code: status.code().unwrap_or(-1),
        })
    }
}
