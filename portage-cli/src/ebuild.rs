use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use bzip2::Compression;
use bzip2::write::BzEncoder;
use camino::{Utf8Path, Utf8PathBuf};
use portage_distfiles::{DistfileResolver, FetchConfig, FetchStatus, Fetcher};
use portage_metadata::SrcUriEntry;
use portage_repo::{
    Ebuild, EbuildEnv, MakeConf, Manifest, Repository, DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF,
};
use portage_vdb::{ContentsEntry, ContentsKind, InstalledPackage, MergeSpec, Vdb};

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

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
            Utf8PathBuf::from(format!("/var/tmp/portage/{}/{pf}", ebuild.category()))
        }
    };

    let mut shell = repo
        .shell()
        .await
        .map_err(|e| Error::Other(format!("creating shell: {e}")))?;

    if let Some(use_val) = read_use_from_make_conf() {
        let flags: Vec<&str> = use_val.split_whitespace().collect();
        shell
            .set_use_flags(&flags)
            .map_err(|e| Error::Other(format!("setting USE flags: {e}")))?;
    }

    for phase in phases {
        run_one_phase(&mut shell, &ebuild, &repo, &repo_root, phase, &work_root, root).await?;
    }

    Ok(())
}

async fn run_one_phase(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    repo: &Repository,
    repo_root: &Utf8Path,
    phase: &str,
    work_root: &Utf8Path,
    root: &Utf8Path,
) -> Result<()> {
    match phase.as_ref() {
        "fetch" => run_fetch(shell, ebuild, repo, work_root).await,
        "clean" => run_clean(work_root),
        "merge" | "qmerge" => {
            run_merge(shell, ebuild, repo_root, work_root, root).await
        }
        _ => shell
            .run_phase(ebuild, phase, work_root.as_std_path(), root.as_std_path())
            .await
            .map_err(|e| Error::Other(format!("phase {phase} failed: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// fetch
// ---------------------------------------------------------------------------

async fn run_fetch(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    repo: &Repository,
    work_root: &Utf8Path,
) -> Result<()> {
    let sourced = shell
        .source_ebuild(ebuild)
        .await
        .map_err(|e| Error::Other(format!("sourcing ebuild: {e}")))?;
    shell.set_a_from_src_uri();

    let src_uri_str = shell.get_var("SRC_URI").unwrap_or_default();
    let distdir = Utf8PathBuf::from(
        shell
            .get_var("DISTDIR")
            .unwrap_or_else(|| "/var/cache/distfiles".into()),
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

    let manifest_path = ebuild
        .path()
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
            Ok(FetchStatus::AlreadyPresent) => {
                println!("fetch: {} (already present)", df.filename)
            }
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
            .run_phase(
                ebuild,
                "nofetch",
                work_root.as_std_path(),
                Path::new("/"),
            )
            .await
            .map_err(|e| Error::Other(format!("pkg_nofetch failed: {e}")))?;
    }

    if any_failed {
        Err(Error::Other(
            "one or more distfiles could not be fetched".into(),
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// merge / qmerge
// ---------------------------------------------------------------------------

async fn run_merge(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    repo_root: &Utf8Path,
    work_root: &Utf8Path,
    root: &Utf8Path,
) -> Result<()> {
    // Ensure temp dir exists so we can write the environment dump there.
    let temp_dir = work_root.join("temp");
    std::fs::create_dir_all(temp_dir.as_std_path())
        .map_err(|e| Error::Other(format!("creating temp dir: {e}")))?;

    // Source ebuild to capture metadata and phase functions.
    shell
        .source_ebuild(ebuild)
        .await
        .map_err(|e| Error::Other(format!("sourcing ebuild: {e}")))?;
    let env = shell.collect_env();

    // Capture environment for environment.bz2 while phase functions are loaded.
    let env_dump = capture_environment(shell, work_root).await;

    // Open (or create) the VDB now so we can query the slot occupant.
    let vdb_root = vdb_root_for(root);
    let vdb = open_or_create_vdb(&vdb_root)?;

    // Detect same-slot replacement: the package currently in this slot (if any).
    let slot_main = env.slot_main().to_owned();
    let old_pkg = vdb
        .find_slot_occupant(&ebuild.cpv().cpn, &slot_main)
        .map_err(|e| Error::Other(format!("slot conflict query failed: {e}")))?
        .filter(|old| old.cpv() != ebuild.cpv()); // ignore re-merge of exact same version

    // Run pkg_preinst for the new package (image dir = $D, target = $ROOT).
    shell
        .run_phase(ebuild, "preinst", work_root.as_std_path(), root.as_std_path())
        .await
        .map_err(|e| Error::Other(format!("pkg_preinst failed: {e}")))?;

    // Copy image → root, build CONTENTS list.
    let image_dir = work_root.join("image");
    let (contents, size) = walk_image(&image_dir, root)?;

    // Collision check — exclude the slot occupant we're about to replace.
    let exclude_cpv = old_pkg.as_ref().map(|p| p.cpv().clone());
    let collisions = vdb
        .find_collisions(&contents, exclude_cpv.as_ref())
        .map_err(|e| Error::Other(format!("collision check failed: {e}")))?;
    if !collisions.is_empty() {
        for c in &collisions {
            eprintln!("collision: {} is already owned by {}", c.path, c.owner);
        }
        return Err(Error::Other(format!(
            "{} file collision(s) detected — aborting merge",
            collisions.len()
        )));
    }

    // Replace the slot occupant if present: prerm → remove unique files →
    // unregister → postrm.
    if let Some(ref old) = old_pkg {
        unmerge_slot_occupant(shell, old, repo_root, work_root, root, &vdb, &contents).await?;
    }

    // Register new VDB entry.
    let build_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let counter = vdb.next_counter()?;
    let spec = merge_spec_from_env(env, ebuild.cpv().clone(), contents, size, build_time, counter);
    let installed = vdb.register(&spec)?;

    // Write environment.bz2 into the VDB entry.
    if let Ok(ref data) = env_dump {
        if let Err(e) = write_environment_bz2(&installed, data) {
            eprintln!("warning: could not write environment.bz2: {e}");
        }
    }

    println!(
        "merge: {}/{}-{} registered (counter={counter})",
        ebuild.category(),
        ebuild.name(),
        ebuild.version()
    );

    // Run pkg_postinst for the new package.
    shell
        .run_phase(ebuild, "postinst", work_root.as_std_path(), root.as_std_path())
        .await
        .map_err(|e| Error::Other(format!("pkg_postinst failed: {e}")))?;

    Ok(())
}

/// Run pkg_prerm on the old slot occupant, remove files unique to it, unregister
/// it from the VDB, then run pkg_postrm.
async fn unmerge_slot_occupant(
    shell: &mut portage_repo::EbuildShell,
    old_pkg: &InstalledPackage,
    repo_root: &Utf8Path,
    work_root: &Utf8Path,
    root: &Utf8Path,
    vdb: &Vdb,
    new_contents: &[ContentsEntry],
) -> Result<()> {
    // Find the old version's ebuild in the repo (it may have been removed from the tree).
    let old_pn = old_pkg.cpv().cpn.package.as_ref();
    let old_pvr = old_pkg.cpv().version.to_string();
    let old_pf = format!("{old_pn}-{old_pvr}");
    let old_ebuild_path = repo_root
        .join(old_pkg.category())
        .join(old_pn)
        .join(format!("{old_pf}.ebuild"));

    let old_ebuild = if old_ebuild_path.exists() {
        match Ebuild::from_path(&old_ebuild_path) {
            Ok(e) => Some(e),
            Err(err) => {
                eprintln!("warning: could not load old ebuild {old_ebuild_path}: {err}");
                None
            }
        }
    } else {
        eprintln!(
            "warning: old ebuild not found at {old_ebuild_path}, skipping pkg_prerm/pkg_postrm"
        );
        None
    };

    // Work root for old package phases (created fresh — prerm/postrm don't need sources).
    let old_work_root = work_root
        .parent()
        .unwrap_or(work_root)
        .join(format!("{old_pf}.old"));
    std::fs::create_dir_all(old_work_root.join("temp").as_std_path())
        .map_err(|e| Error::Other(format!("creating old work root: {e}")))?;

    // pkg_prerm — try loading from environment.bz2 if ebuild is gone.
    let old_sourced = match &old_ebuild {
        Some(e) => {
            shell
                .run_phase(e, "prerm", old_work_root.as_std_path(), root.as_std_path())
                .await
                .map_err(|e| Error::Other(format!("pkg_prerm failed: {e}")))?;
            true
        }
        None => {
            try_run_phase_from_env_bz2(shell, old_pkg, "prerm", &old_work_root, root).await
        }
    };

    // Remove files that belong only to the old package (new package will not replace them).
    let old_contents = old_pkg
        .contents()
        .map_err(|e| Error::Other(format!("reading old CONTENTS: {e}")))?;
    remove_old_unique_files(&old_contents, new_contents, root)?;

    // Unregister old VDB entry.
    vdb.unregister(old_pkg)
        .map_err(|e| Error::Other(format!("unregistering old package: {e}")))?;

    // pkg_postrm.
    if old_sourced {
        match &old_ebuild {
            Some(e) => {
                shell
                    .run_phase(e, "postrm", old_work_root.as_std_path(), root.as_std_path())
                    .await
                    .map_err(|e| Error::Other(format!("pkg_postrm failed: {e}")))?;
            }
            None => {
                let _ = try_run_phase_from_env_bz2(
                    shell,
                    old_pkg,
                    "postrm",
                    &old_work_root,
                    root,
                )
                .await;
            }
        }
    }

    // Clean up old work root (best effort).
    let _ = std::fs::remove_dir_all(old_work_root.as_std_path());

    Ok(())
}

/// Attempt to source `environment.bz2` from the VDB entry and run a phase.
///
/// Returns `true` if the environment was loaded and the phase ran, `false` if
/// `environment.bz2` is absent or could not be decompressed (warning is printed).
async fn try_run_phase_from_env_bz2(
    shell: &mut portage_repo::EbuildShell,
    pkg: &InstalledPackage,
    phase: &str,
    work_root: &Utf8Path,
    root: &Utf8Path,
) -> bool {
    let env_bz2 = pkg.path().join("environment.bz2");
    if !env_bz2.exists() {
        return false;
    }

    // Decompress to a temp file then source it.
    let temp_env = work_root.join("temp/environment.old");
    let compressed = match std::fs::read(env_bz2.as_std_path()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("warning: could not read environment.bz2: {e}");
            return false;
        }
    };
    let decompressed = match decompress_bzip2(&compressed) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("warning: could not decompress environment.bz2: {e}");
            return false;
        }
    };
    if let Err(e) = std::fs::write(temp_env.as_std_path(), &decompressed) {
        eprintln!("warning: could not write temp environment: {e}");
        return false;
    }

    let source_cmd = format!(". '{}'", temp_env.as_str().replace('\'', "'\\''"));
    if shell.run_string(&source_cmd).await.is_err() {
        eprintln!("warning: could not source saved environment");
        return false;
    }

    // Run the phase function directly (it's now defined in the shell).
    let func = match phase {
        "prerm" => "pkg_prerm",
        "postrm" => "pkg_postrm",
        other => other,
    };

    // Set up the minimal variables needed by pkg_* phases.
    let root_str = {
        let s = root.as_str();
        if s.ends_with('/') { s.to_owned() } else { format!("{s}/") }
    };
    if let Err(e) = shell
        .run_string(&format!(
            "ROOT='{root_str}' EROOT='{root_str}' EBUILD_PHASE_FUNC='{func}' {func}"
        ))
        .await
    {
        eprintln!("warning: {func} from environment.bz2 failed: {e}");
    }

    true
}

/// Remove files that are in `old_contents` but NOT in `new_contents`.
///
/// Files/symlinks common to both are left in place (they were overwritten
/// during `walk_image`). Directories are removed only if empty.
/// All removals are best-effort — warnings are printed on failure.
fn remove_old_unique_files(
    old_contents: &[ContentsEntry],
    new_contents: &[ContentsEntry],
    root: &Utf8Path,
) -> Result<()> {
    let new_paths: HashSet<&Utf8PathBuf> = new_contents.iter().map(|e| &e.path).collect();

    // Reverse order: files before their parent directories.
    for entry in old_contents.iter().rev() {
        if new_paths.contains(&entry.path) {
            continue;
        }
        let rel = entry.path.strip_prefix("/").unwrap_or(&entry.path);
        let dest = root.join(rel);

        match entry.kind {
            ContentsKind::Obj | ContentsKind::Sym => {
                if dest.exists() || std::fs::symlink_metadata(dest.as_std_path()).is_ok() {
                    if let Err(e) = std::fs::remove_file(dest.as_std_path()) {
                        eprintln!("warning: could not remove {dest}: {e}");
                    }
                }
            }
            ContentsKind::Dir => {
                // Only remove if empty — other packages may share the directory.
                let _ = std::fs::remove_dir(dest.as_std_path());
            }
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// clean
// ---------------------------------------------------------------------------

fn run_clean(work_root: &Utf8Path) -> Result<()> {
    if work_root.exists() {
        std::fs::remove_dir_all(work_root)
            .map_err(|e| Error::Other(format!("cleaning {work_root}: {e}")))?;
        println!("clean: removed {work_root}");
    } else {
        println!("clean: {work_root} does not exist, nothing to do");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// image walk
// ---------------------------------------------------------------------------

fn walk_image(image_dir: &Utf8Path, dest_root: &Utf8Path) -> Result<(Vec<ContentsEntry>, u64)> {
    if !image_dir.exists() {
        return Ok((vec![], 0));
    }

    let mut contents: Vec<ContentsEntry> = Vec::new();
    let mut total_size: u64 = 0;
    let mut queue: std::collections::VecDeque<Utf8PathBuf> = std::collections::VecDeque::new();
    queue.push_back(image_dir.to_path_buf());

    while let Some(dir) = queue.pop_front() {
        let read_dir = std::fs::read_dir(dir.as_std_path())
            .map_err(|e| Error::Other(format!("reading image dir {dir}: {e}")))?;

        for entry in read_dir {
            let entry =
                entry.map_err(|e| Error::Other(format!("reading dir entry: {e}")))?;
            let src_path: Utf8PathBuf = entry
                .path()
                .try_into()
                .map_err(|_| Error::Other("non-UTF-8 path in image".into()))?;

            let rel = src_path
                .strip_prefix(image_dir)
                .map_err(|_| Error::Other(format!("path escape: {src_path}")))?;
            let installed = Utf8PathBuf::from("/").join(rel);
            let dest_path = dest_root.join(rel);

            let meta = std::fs::symlink_metadata(src_path.as_std_path())
                .map_err(|e| Error::Other(format!("stat {src_path}: {e}")))?;

            if meta.file_type().is_symlink() {
                let raw_target = std::fs::read_link(src_path.as_std_path())
                    .map_err(|e| Error::Other(format!("readlink {src_path}: {e}")))?;
                let target: Utf8PathBuf = raw_target
                    .try_into()
                    .map_err(|_| Error::Other("non-UTF-8 symlink target".into()))?;
                if dest_path.exists()
                    || std::fs::symlink_metadata(dest_path.as_std_path()).is_ok()
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
                if let Some(parent) = dest_path.parent() {
                    std::fs::create_dir_all(parent.as_std_path())
                        .map_err(|e| Error::Other(format!("mkdir {parent}: {e}")))?;
                }
                std::fs::copy(src_path.as_std_path(), dest_path.as_std_path())
                    .map_err(|e| Error::Other(format!("copy {src_path} → {dest_path}: {e}")))?;
                std::fs::set_permissions(dest_path.as_std_path(), meta.permissions())
                    .map_err(|e| Error::Other(format!("chmod {dest_path}: {e}")))?;

                total_size += meta.len();
                let data = std::fs::read(dest_path.as_std_path())
                    .map_err(|e| Error::Other(format!("reading {dest_path}: {e}")))?;
                let md5_str = format!("{:x}", md5::compute(&data));
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
        }
    }

    Ok((contents, total_size))
}

// ---------------------------------------------------------------------------
// Environment capture (Gap 3)
// ---------------------------------------------------------------------------

/// Dump all shell variables and functions to a temp file and return the raw bytes.
///
/// Used to populate `environment.bz2` in the VDB entry so that `pkg_prerm` /
/// `pkg_postrm` can run later even if the ebuild has been removed from the tree.
async fn capture_environment(
    shell: &mut portage_repo::EbuildShell,
    work_root: &Utf8Path,
) -> std::result::Result<Vec<u8>, String> {
    let dump_path = work_root.join("temp/environment");
    // Escape single quotes in the path (unlikely but safe).
    let path_escaped = dump_path.as_str().replace('\'', "'\\''");
    shell
        .run_string(&format!(
            "{{ declare -p; declare -f; }} > '{path_escaped}' 2>/dev/null || true"
        ))
        .await
        .map_err(|e| format!("environment capture failed: {e}"))?;
    std::fs::read(dump_path.as_std_path()).map_err(|e| format!("reading env dump: {e}"))
}

/// Write `environment.bz2` into the VDB directory for `pkg`.
fn write_environment_bz2(pkg: &InstalledPackage, env_data: &[u8]) -> Result<()> {
    use std::io::Write;

    let path = pkg.path().join("environment.bz2");
    let mut encoder = BzEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(env_data)
        .map_err(|e| Error::Other(format!("compressing environment: {e}")))?;
    let compressed = encoder
        .finish()
        .map_err(|e| Error::Other(format!("finalizing bzip2: {e}")))?;
    std::fs::write(path.as_std_path(), compressed)
        .map_err(|e| Error::Other(format!("writing environment.bz2: {e}")))
}

/// Decompress a bzip2 blob and return the raw bytes.
fn decompress_bzip2(data: &[u8]) -> std::result::Result<Vec<u8>, String> {
    use bzip2::read::BzDecoder;
    use std::io::Read;

    let mut decoder = BzDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| format!("bzip2 decompress: {e}"))?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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

        fs::create_dir_all(image.join("usr/bin").as_std_path()).unwrap();
        fs::write(image.join("usr/bin/testprog").as_std_path(), b"#!/bin/sh\n").unwrap();
        symlink("testprog", image.join("usr/bin/tp").as_std_path()).unwrap();
        fs::create_dir_all(root.as_std_path()).unwrap();

        let (contents, size) = walk_image(&image, &root).unwrap();

        assert!(root.join("usr/bin/testprog").exists());
        assert!(root
            .join("usr/bin/tp")
            .as_std_path()
            .symlink_metadata()
            .is_ok());

        let dirs: Vec<_> = contents
            .iter()
            .filter(|e| e.kind == ContentsKind::Dir)
            .collect();
        let objs: Vec<_> = contents
            .iter()
            .filter(|e| e.kind == ContentsKind::Obj)
            .collect();
        let syms: Vec<_> = contents
            .iter()
            .filter(|e| e.kind == ContentsKind::Sym)
            .collect();
        assert!(!dirs.is_empty());
        assert_eq!(objs.len(), 1);
        assert_eq!(syms.len(), 1);
        assert_eq!(objs[0].path, Utf8PathBuf::from("/usr/bin/testprog"));
        assert!(objs[0].md5.is_some());
        assert_eq!(syms[0].path, Utf8PathBuf::from("/usr/bin/tp"));
        assert_eq!(syms[0].target.as_deref(), Some(Utf8Path::new("testprog")));
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

    #[test]
    fn remove_old_unique_files_removes_only_unique() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::try_from(tmp.path().to_owned()).unwrap();

        // Create files that simulate installed content.
        fs::create_dir_all(root.join("usr/bin").as_std_path()).unwrap();
        fs::write(root.join("usr/bin/old-only").as_std_path(), b"old").unwrap();
        fs::write(root.join("usr/bin/shared").as_std_path(), b"shared").unwrap();

        let old_contents = vec![
            ContentsEntry {
                kind: ContentsKind::Dir,
                path: "/usr/bin".into(),
                md5: None,
                mtime: None,
                target: None,
            },
            ContentsEntry {
                kind: ContentsKind::Obj,
                path: "/usr/bin/old-only".into(),
                md5: Some("aa".into()),
                mtime: Some(0),
                target: None,
            },
            ContentsEntry {
                kind: ContentsKind::Obj,
                path: "/usr/bin/shared".into(),
                md5: Some("bb".into()),
                mtime: Some(0),
                target: None,
            },
        ];
        let new_contents = vec![ContentsEntry {
            kind: ContentsKind::Obj,
            path: "/usr/bin/shared".into(),
            md5: Some("cc".into()),
            mtime: Some(1),
            target: None,
        }];

        remove_old_unique_files(&old_contents, &new_contents, &root).unwrap();

        // old-only should be gone, shared should still be there.
        assert!(!root.join("usr/bin/old-only").exists());
        assert!(root.join("usr/bin/shared").exists());
        // Dir should still exist (not empty — shared is still there).
        assert!(root.join("usr/bin").exists());
    }
}
