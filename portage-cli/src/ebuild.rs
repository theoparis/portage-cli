use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use camino::{Utf8Path, Utf8PathBuf};
use portage_distfiles::{DistfileResolver, FetchConfig, FetchStatus, Fetcher};
use portage_metadata::SrcUriEntry;
use portage_repo::{Ebuild, EbuildEnv, MakeConf, Manifest, Repository, DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF};
use portage_vdb::{ContentsEntry, ContentsKind, MergeSpec, Vdb};

use crate::error::{Error, Result};

/// Execute one or more ebuild phases for a given `.ebuild` file.
pub async fn run(
    ebuild_path: &str,
    phases: &[String],
    work_dir: Option<&Utf8Path>,
    repo_override: Option<&str>,
    root: &Utf8Path,
) -> Result<()> {
    let path = Utf8Path::new(ebuild_path);
    let ebuild = Ebuild::from_path(path)
        .map_err(|e| Error::Other(format!("loading {ebuild_path}: {e}")))?;

    let repo_root = match repo_override {
        Some(r) => Utf8PathBuf::from(r),
        None => ebuild
            .repo_root()
            .ok_or_else(|| Error::Other("cannot determine repo root from ebuild path".into()))?
            .to_owned(),
    };

    let repo = Repository::open(repo_root.as_std_path())
        .map_err(|e| Error::Other(format!("opening repo at {repo_root}: {e}")))?;

    let work_root = match work_dir {
        Some(p) => p.to_owned(),
        None => {
            let pf = format!("{}-{}", ebuild.name(), ebuild.version());
            Utf8PathBuf::from(format!(
                "/var/tmp/portage/{}/{pf}",
                ebuild.category()
            ))
        }
    };

    let mut shell = repo
        .shell()
        .await
        .map_err(|e| Error::Other(format!("creating shell: {e}")))?;

    // Apply global USE flags from make.conf if available.
    if let Some(use_val) = read_use_from_make_conf() {
        let flags: Vec<&str> = use_val.split_whitespace().collect();
        shell
            .set_use_flags(&flags)
            .map_err(|e| Error::Other(format!("setting USE flags: {e}")))?;
    }

    for phase in phases {
        run_one_phase(&mut shell, &ebuild, &repo, phase, &work_root, root).await?;
    }

    Ok(())
}

async fn run_one_phase(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    repo: &Repository,
    phase: &str,
    work_root: &Utf8Path,
    root: &Utf8Path,
) -> Result<()> {
    match phase.as_ref() {
        "fetch" => run_fetch_stub(shell, ebuild, repo, work_root).await,
        "clean" => run_clean(work_root),
        "merge" | "qmerge" => run_merge(shell, ebuild, repo, work_root, root).await,
        _ => shell
            .run_phase(ebuild, phase, work_root.as_std_path())
            .await
            .map_err(|e| Error::Other(format!("phase {phase} failed: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// fetch
// ---------------------------------------------------------------------------

async fn run_fetch_stub(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    repo: &Repository,
    work_root: &Utf8Path,
) -> Result<()> {
    // Source the ebuild to populate SRC_URI, then compute $A.
    let sourced = shell
        .source_ebuild(ebuild)
        .await
        .map_err(|e| Error::Other(format!("sourcing ebuild: {e}")))?;
    shell.set_a_from_src_uri();

    let src_uri_str = shell.get_var("SRC_URI").unwrap_or_default();
    let distdir = Utf8PathBuf::from(
        shell.get_var("DISTDIR").unwrap_or_else(|| "/var/cache/distfiles".into()),
    );

    if src_uri_str.trim().is_empty() {
        println!("fetch: nothing to fetch (SRC_URI is empty)");
        return Ok(());
    }

    let entries = SrcUriEntry::parse(&src_uri_str)
        .map_err(|e| Error::Other(format!("parsing SRC_URI: {e}")))?;

    let use_flags: HashSet<String> = shell
        .get_var("USE")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();

    let gentoo_mirrors = gentoo_mirrors_list();
    let resolver = DistfileResolver::from_repo(repo, gentoo_mirrors)
        .map_err(|e| Error::Other(format!("loading mirrors: {e}")))?;
    let distfiles = resolver.resolve(&entries, &use_flags);

    if distfiles.is_empty() {
        println!("fetch: nothing to fetch");
        return Ok(());
    }

    let manifest_path = ebuild.path()
        .parent()
        .map(|p| p.join("Manifest"))
        .filter(|p| p.exists());
    let manifest = match manifest_path {
        Some(ref p) => {
            let raw = std::fs::read_to_string(p)
                .map_err(|e| Error::Other(format!("reading Manifest: {e}")))?;
            Manifest::parse(&raw)
                .map_err(|e| Error::Other(format!("parsing Manifest: {e}")))?
        }
        None => Manifest { entries: vec![] },
    };

    let (fetch_cmd, resume_cmd) = read_fetch_commands();
    let config = FetchConfig::from_make_conf(fetch_cmd, resume_cmd);
    let fetcher = Fetcher::new(distdir.clone(), config);

    std::fs::create_dir_all(distdir.as_std_path())
        .map_err(|e| Error::Other(format!("creating distdir {distdir}: {e}")))?;

    let results = fetcher.fetch_all(&distfiles, &manifest).await;

    let mut any_failed = false;
    let mut any_restricted = false;
    for (df, result) in results {
        match result {
            Ok(FetchStatus::AlreadyPresent) => println!("fetch: {} (already present)", df.filename),
            Ok(FetchStatus::Downloaded) => println!("fetch: {} ok", df.filename),
            Ok(FetchStatus::FetchRestricted) => {
                eprintln!("fetch: {} is fetch-restricted (RESTRICT=fetch)", df.filename);
                any_restricted = true;
            }
            Err(e) => {
                eprintln!("fetch: {} failed: {e}", df.filename);
                any_failed = true;
            }
        }
    }

    let _ = sourced;

    if any_restricted || any_failed {
        shell
            .run_phase(ebuild, "nofetch", work_root.as_std_path())
            .await
            .map_err(|e| Error::Other(format!("pkg_nofetch failed: {e}")))?;
    }

    if any_failed {
        Err(Error::Other("one or more distfiles could not be fetched".into()))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// merge / qmerge
// ---------------------------------------------------------------------------

/// Copy files from the image directory into `root`, build CONTENTS, register
/// in the VDB, and run pkg_preinst / pkg_postinst.
async fn run_merge(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    _repo: &Repository,
    work_root: &Utf8Path,
    root: &Utf8Path,
) -> Result<()> {
    // Source ebuild to collect metadata before preinst touches the shell.
    shell
        .source_ebuild(ebuild)
        .await
        .map_err(|e| Error::Other(format!("sourcing ebuild: {e}")))?;

    let env = shell.collect_env();

    // Run pkg_preinst (image dir is $D, real root is $ROOT).
    shell
        .run_phase(ebuild, "preinst", work_root.as_std_path())
        .await
        .map_err(|e| Error::Other(format!("pkg_preinst failed: {e}")))?;

    // Copy image → root and build CONTENTS.
    let image_dir = work_root.join("image");
    let (contents, size) = walk_image(&image_dir, root)?;

    // Check for file ownership conflicts before touching the VDB.
    let vdb_root = vdb_root_for(root);
    let vdb = open_or_create_vdb(&vdb_root)?;
    let collisions = vdb
        .find_collisions(&contents, None)
        .map_err(|e| Error::Other(format!("collision check failed: {e}")))?;
    if !collisions.is_empty() {
        for c in &collisions {
            eprintln!(
                "collision: {} is already owned by {}",
                c.path, c.owner
            );
        }
        return Err(Error::Other(format!(
            "{} file collision(s) detected — aborting merge",
            collisions.len()
        )));
    }

    let build_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let counter = vdb.next_counter()?;

    let cpv = ebuild.cpv().clone();
    let spec = merge_spec_from_env(env, cpv, contents, size, build_time, counter);

    vdb.register(&spec)?;

    println!(
        "merge: {}/{}-{} registered (counter={counter})",
        ebuild.category(),
        ebuild.name(),
        ebuild.version()
    );

    // Run pkg_postinst.
    shell
        .run_phase(ebuild, "postinst", work_root.as_std_path())
        .await
        .map_err(|e| Error::Other(format!("pkg_postinst failed: {e}")))?;

    Ok(())
}

/// Build a [`MergeSpec`] from a collected [`EbuildEnv`] and install-time data.
fn merge_spec_from_env(
    env: EbuildEnv,
    cpv: portage_atom::Cpv,
    contents: Vec<ContentsEntry>,
    size: u64,
    build_time: u64,
    counter: u64,
) -> MergeSpec {
    MergeSpec {
        cpv,
        eapi: env.eapi,
        slot: env.slot,
        use_flags: env.use_flags,
        iuse: env.iuse,
        depend: env.depend,
        rdepend: env.rdepend,
        bdepend: env.bdepend,
        pdepend: env.pdepend,
        idepend: env.idepend,
        keywords: env.keywords,
        license: env.license,
        description: env.description,
        homepage: env.homepage,
        restrict: env.restrict,
        properties: env.properties,
        defined_phases: env.defined_phases,
        repository: env.repository,
        contents,
        build_time,
        size,
        counter,
    }
}

/// Walk `image_dir`, copy each entry under `dest_root`, and return the CONTENTS
/// list and total installed size in bytes.
///
/// Directories are created if absent.  Regular files are copied with their
/// source permissions.  Symlinks are recreated as-is (target is not resolved).
fn walk_image(image_dir: &Utf8Path, dest_root: &Utf8Path) -> Result<(Vec<ContentsEntry>, u64)> {
    if !image_dir.exists() {
        return Ok((vec![], 0));
    }

    let mut contents: Vec<ContentsEntry> = Vec::new();
    let mut total_size: u64 = 0;

    // BFS queue of directories to process.
    let mut queue: std::collections::VecDeque<Utf8PathBuf> = std::collections::VecDeque::new();
    queue.push_back(image_dir.to_path_buf());

    while let Some(dir) = queue.pop_front() {
        let read_dir = std::fs::read_dir(dir.as_std_path())
            .map_err(|e| Error::Other(format!("reading image dir {dir}: {e}")))?;

        for entry in read_dir {
            let entry = entry
                .map_err(|e| Error::Other(format!("reading dir entry: {e}")))?;
            let src_path: Utf8PathBuf = entry
                .path()
                .try_into()
                .map_err(|_| Error::Other("non-UTF-8 path in image".into()))?;

            // The installed path is always absolute (relative to "/").
            let rel = src_path
                .strip_prefix(image_dir)
                .map_err(|_| Error::Other(format!("path escape: {src_path}")))?;
            let installed = Utf8PathBuf::from("/").join(rel);
            let dest_path = dest_root.join(rel);

            // Use symlink_metadata so we see the symlink, not its target.
            let meta = std::fs::symlink_metadata(src_path.as_std_path())
                .map_err(|e| Error::Other(format!("stat {src_path}: {e}")))?;

            if meta.file_type().is_symlink() {
                let raw_target = std::fs::read_link(src_path.as_std_path())
                    .map_err(|e| Error::Other(format!("readlink {src_path}: {e}")))?;
                let target: Utf8PathBuf = raw_target
                    .try_into()
                    .map_err(|_| Error::Other("non-UTF-8 symlink target".into()))?;

                // Remove stale symlink if present; then create the new one.
                if dest_path.exists() || std::fs::symlink_metadata(dest_path.as_std_path()).is_ok()
                {
                    std::fs::remove_file(dest_path.as_std_path())
                        .map_err(|e| Error::Other(format!("removing {dest_path}: {e}")))?;
                }
                std::os::unix::fs::symlink(target.as_std_path(), dest_path.as_std_path())
                    .map_err(|e| Error::Other(format!("symlink {dest_path}: {e}")))?;

                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                contents.push(ContentsEntry {
                    kind: ContentsKind::Sym,
                    path: installed,
                    md5: None,
                    mtime,
                    target: Some(target),
                });
            } else if meta.is_dir() {
                std::fs::create_dir_all(dest_path.as_std_path())
                    .map_err(|e| Error::Other(format!("mkdir {dest_path}: {e}")))?;
                contents.push(ContentsEntry {
                    kind: ContentsKind::Dir,
                    path: installed,
                    md5: None,
                    mtime: None,
                    target: None,
                });
                queue.push_back(src_path);
            } else if meta.is_file() {
                // Ensure parent exists in dest.
                if let Some(parent) = dest_path.parent() {
                    std::fs::create_dir_all(parent.as_std_path())
                        .map_err(|e| Error::Other(format!("mkdir {parent}: {e}")))?;
                }
                std::fs::copy(src_path.as_std_path(), dest_path.as_std_path())
                    .map_err(|e| Error::Other(format!("copy {src_path} → {dest_path}: {e}")))?;

                // Preserve source permissions.
                std::fs::set_permissions(
                    dest_path.as_std_path(),
                    meta.permissions(),
                )
                .map_err(|e| Error::Other(format!("chmod {dest_path}: {e}")))?;

                let file_size = meta.len();
                total_size += file_size;

                // Compute MD5 of the installed file (portage uses MD5 in CONTENTS).
                let data = std::fs::read(dest_path.as_std_path())
                    .map_err(|e| Error::Other(format!("reading {dest_path}: {e}")))?;
                let digest = md5::compute(&data);
                let md5_str = format!("{digest:x}");

                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                contents.push(ContentsEntry {
                    kind: ContentsKind::Obj,
                    path: installed,
                    md5: Some(md5_str),
                    mtime,
                    target: None,
                });
            }
            // FIFOs and device nodes: record but don't copy (rare in ebuilds).
        }
    }

    Ok((contents, total_size))
}

// ---------------------------------------------------------------------------
// clean
// ---------------------------------------------------------------------------

fn run_clean(work_root: &Utf8Path) -> Result<()> {
    if work_root.exists() {
        std::fs::remove_dir_all(work_root).map_err(|e| {
            Error::Other(format!("cleaning {work_root}: {e}"))
        })?;
        println!("clean: removed {work_root}");
    } else {
        println!("clean: {work_root} does not exist, nothing to do");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Derive VDB root from the installation root.
/// When root is "/", VDB lives at "/var/db/pkg"; otherwise at "{root}/var/db/pkg".
fn vdb_root_for(root: &Utf8Path) -> Utf8PathBuf {
    if root.as_str() == "/" {
        Utf8PathBuf::from("/var/db/pkg")
    } else {
        root.join("var/db/pkg")
    }
}

fn open_or_create_vdb(path: &Utf8Path) -> Result<Vdb> {
    if !path.exists() {
        std::fs::create_dir_all(path.as_std_path())
            .map_err(|e| Error::Other(format!("creating VDB at {path}: {e}")))?;
    }
    Vdb::open(path).map_err(|e| Error::Other(format!("opening VDB at {path}: {e}")))
}

fn gentoo_mirrors_list() -> Vec<String> {
    if let Ok(val) = std::env::var("GENTOO_MIRRORS") {
        if !val.trim().is_empty() {
            return val.split_whitespace().map(str::to_owned).collect();
        }
    }
    for candidate in [DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF] {
        let p = Utf8Path::new(candidate);
        if p.exists() {
            if let Ok(mc) = MakeConf::load(p) {
                if let Some(val) = mc.get("GENTOO_MIRRORS") {
                    return val.split_whitespace().map(str::to_owned).collect();
                }
            }
        }
    }
    vec![]
}

fn read_fetch_commands() -> (Option<String>, Option<String>) {
    for candidate in [DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF] {
        let p = Utf8Path::new(candidate);
        if p.exists() {
            if let Ok(mc) = MakeConf::load(p) {
                let fetch = mc.get("FETCHCOMMAND").map(str::to_owned);
                let resume = mc.get("RESUMECOMMAND").map(str::to_owned);
                if fetch.is_some() || resume.is_some() {
                    return (fetch, resume);
                }
            }
        }
    }
    (None, None)
}

fn read_use_from_make_conf() -> Option<String> {
    for candidate in [DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF] {
        let p = Utf8Path::new(candidate);
        if p.exists() {
            if let Ok(mc) = MakeConf::load(p) {
                if let Some(val) = mc.get("USE") {
                    return Some(val.to_owned());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_vdb::ContentsKind;
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn walk_image_copies_files_and_builds_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("image")).unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();

        // Build a small image tree.
        fs::create_dir_all(image.join("usr/bin").as_std_path()).unwrap();
        fs::write(image.join("usr/bin/testprog").as_std_path(), b"#!/bin/sh\n").unwrap();
        symlink("testprog", image.join("usr/bin/tp").as_std_path()).unwrap();
        fs::create_dir_all(root.as_std_path()).unwrap();

        let (contents, size) = walk_image(&image, &root).unwrap();

        // Files should have been copied.
        assert!(root.join("usr/bin/testprog").exists());
        assert!(root.join("usr/bin/tp").as_std_path().symlink_metadata().is_ok());

        // CONTENTS should have dir + obj + sym entries.
        let dirs: Vec<_> = contents.iter().filter(|e| e.kind == ContentsKind::Dir).collect();
        let objs: Vec<_> = contents.iter().filter(|e| e.kind == ContentsKind::Obj).collect();
        let syms: Vec<_> = contents.iter().filter(|e| e.kind == ContentsKind::Sym).collect();
        assert!(!dirs.is_empty(), "expected at least one dir entry");
        assert_eq!(objs.len(), 1);
        assert_eq!(syms.len(), 1);
        assert_eq!(objs[0].path, Utf8PathBuf::from("/usr/bin/testprog"));
        assert!(objs[0].md5.is_some(), "obj entry should have MD5");
        assert_eq!(syms[0].path, Utf8PathBuf::from("/usr/bin/tp"));
        assert_eq!(syms[0].target.as_deref(), Some(Utf8Path::new("testprog")));

        // Size should be > 0.
        assert!(size > 0);
    }

    #[test]
    fn walk_image_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("image")).unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();
        fs::create_dir_all(image.as_std_path()).unwrap();
        fs::create_dir_all(root.as_std_path()).unwrap();

        let (contents, size) = walk_image(&image, &root).unwrap();
        assert!(contents.is_empty());
        assert_eq!(size, 0);
    }

    #[test]
    fn walk_image_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let image = Utf8PathBuf::try_from(tmp.path().join("no-such-image")).unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().join("root")).unwrap();
        let (contents, size) = walk_image(&image, &root).unwrap();
        assert!(contents.is_empty());
        assert_eq!(size, 0);
    }
}
