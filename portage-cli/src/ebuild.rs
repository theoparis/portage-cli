use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::{Ebuild, MakeConf, Repository, DEFAULT_MAKE_CONF, LEGACY_MAKE_CONF};

use crate::error::{Error, Result};

/// Execute one or more ebuild phases for a given `.ebuild` file.
pub async fn run(
    ebuild_path: &str,
    phases: &[String],
    work_dir: Option<&Utf8Path>,
    repo_override: Option<&str>,
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
        run_one_phase(&mut shell, &ebuild, phase, &work_root).await?;
    }

    Ok(())
}

async fn run_one_phase(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
    phase: &str,
    work_root: &Utf8Path,
) -> Result<()> {
    match phase.as_ref() {
        "fetch" => run_fetch_stub(shell, ebuild).await,
        "clean" => run_clean(work_root),
        "merge" | "qmerge" => {
            eprintln!("em ebuild: '{phase}' is not yet implemented");
            Err(Error::NotImplemented(format!("ebuild {phase}")))
        }
        _ => shell
            .run_phase(ebuild, phase, work_root.as_std_path())
            .await
            .map_err(|e| Error::Other(format!("phase {phase} failed: {e}"))),
    }
}

/// Stub for the `fetch` phase: print the distfiles that would be downloaded.
///
/// Full network fetch is not yet implemented. Prints the filenames from `$A`
/// so the user can fetch them manually into DISTDIR.
async fn run_fetch_stub(
    shell: &mut portage_repo::EbuildShell,
    ebuild: &Ebuild,
) -> Result<()> {
    // Source the ebuild to populate SRC_URI (without running any phase).
    // Then compute $A explicitly — run_phase does this internally, but for
    // fetch we only source, so we must call it ourselves.
    shell
        .source_ebuild(ebuild)
        .await
        .map_err(|e| Error::Other(format!("sourcing ebuild: {e}")))?;
    shell.set_a_from_src_uri();

    let a = shell.get_var("A").unwrap_or_default();
    let distdir = shell.get_var("DISTDIR").unwrap_or_else(|| "/var/cache/distfiles".into());

    if a.trim().is_empty() {
        println!("fetch: nothing to fetch (SRC_URI is empty)");
        return Ok(());
    }

    eprintln!("fetch: not yet implemented — please fetch the following into {distdir}:");
    for file in a.split_whitespace() {
        eprintln!("  {file}");
    }

    Ok(())
}

/// Remove the work directory tree (`clean` phase equivalent).
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
