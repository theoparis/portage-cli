mod cli;
mod ebuild;
mod error;
mod maint;
mod pkg;
mod postprocess;
mod query;
mod regen;
mod search;
mod use_flags;
mod vdb;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::str::FromStr;

use anyhow::{Context, bail};
use clap::Parser;
use error::Result;

use cli::{Applet, CleanTarget, GlsaCommand, LogCommand, MaintCommand, NewsCommand, QueryCommand};

fn parse_atoms(raw: &[String]) -> Vec<portage_atom::Dep> {
    raw.iter()
        .filter_map(|s| match portage_atom::Dep::from_str(s) {
            Ok(dep) => Some(dep),
            Err(e) => {
                eprintln!("warning: skipping invalid atom '{}': {}", s, e);
                None
            }
        })
        .collect()
}

#[tokio::main]
async fn main() {
    let cli = cli::Cli::parse();
    cli.color.write_global();

    let result = match &cli.applet {
        Some(applet) => run_applet(applet, &cli).await,
        None => {
            if cli.atoms.is_empty() {
                eprintln!("em: no atoms or applet specified. Use --help for usage.");
                std::process::exit(1);
            }
            run_emerge(&cli).await
        }
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run_emerge(cli: &cli::Cli) -> Result<()> {
    // emerge -s / -S: the arguments are search patterns, not atoms.
    if cli.search || cli.searchdesc {
        return search::run_emerge_style(&cli.search_repos(), &cli.atoms, cli.searchdesc).await;
    }
    let resolved = cli.repo_path();
    let repo_path = camino::Utf8Path::new(&resolved);
    if !repo_path.is_dir() {
        bail!("repo not found at {resolved}");
    }
    let repo = portage_repo::Repository::open(repo_path.as_std_path())?;
    let vdb = open_vdb(cli).ok();
    let mode = if cli.update {
        query::ResolveMode::PreferInstalled
    } else {
        query::ResolveMode::Error
    };
    let parsed = query::resolve_atoms(&cli.atoms, &repo, vdb.as_ref(), mode);
    let atoms: Vec<String> = parsed.iter().map(|d| d.to_string()).collect();
    if atoms.is_empty() {
        bail!("em: no valid atoms");
    }
    let root = cli.root.as_deref().map(camino::Utf8Path::new);
    let format = if cli.tree {
        cli::DepgraphFormat::Tree
    } else {
        cli::DepgraphFormat::Pretty
    };
    let outcome = query::depgraph::depgraph(query::depgraph::DepgraphOpts {
        repo_path,
        atoms: &atoms,
        arch: &cli.arch,
        format,
        verbose: cli.verbose,
        empty: cli.emptytree,
        autounmask: cli.autounmask,
        autounmask_write: cli.autounmask_write,
        autosolve_use: cli.autosolve_use,
        multi_repo: cli.repo.is_none(),
        root,
    })
    .await?;

    if cli.pretend {
        if outcome.exit_code != 0 {
            std::process::exit(outcome.exit_code);
        }
        return Ok(());
    }

    // em <atoms>: build and merge the resolved plan, in order.
    if outcome.exit_code != 0 {
        bail!("configuration changes are required (see above) — refusing to merge");
    }
    let prefix = cli.prefix.as_deref().map(camino::Utf8Path::new);
    let merge_root = prefix
        .or(root)
        .unwrap_or_else(|| camino::Utf8Path::new("/"))
        .to_owned();
    let distdir = prefix.map(|p| p.join("var/cache/distfiles"));
    let work_base = ebuild::default_work_base(prefix);

    if outcome.plan.is_empty() {
        return Ok(());
    }

    if cli.ask && !confirm_merge(outcome.plan.len())? {
        println!(">>> Quitting.");
        return Ok(());
    }

    run_merge_plan(
        &outcome.plan,
        &merge_root,
        &work_base,
        distdir.as_deref(),
        cli.quiet,
        cli.keep_going,
        cli.emptytree,
    )
    .await
}

/// One package's merge failure, for the end-of-run report.
struct MergeFailure {
    cpv: String,
    log: camino::Utf8PathBuf,
    cause: String,
}

/// Prompt before merging (`--ask`). Defaults to no on empty input or EOF.
fn confirm_merge(count: usize) -> Result<bool> {
    use std::io::Write;
    print!("\n>>> Would you like to merge these {count} package(s)? [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Ok(false);
    }
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}

/// Build and merge a resolved plan in install order.
///
/// Resume comes for free from the target VDB: a package already recorded
/// there at the planned version is skipped (a previous run merged it), so
/// re-running after an interruption continues from the first unmerged entry
/// without a separate state file. `--emptytree` forces every entry to rebuild.
async fn run_merge_plan(
    plan: &[query::depgraph::PlannedMerge],
    merge_root: &camino::Utf8Path,
    work_base: &camino::Utf8Path,
    distdir: Option<&camino::Utf8Path>,
    quiet: bool,
    keep_going: bool,
    emptytree: bool,
) -> Result<()> {
    let total = plan.len();
    let mut merged = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<MergeFailure> = Vec::new();

    for (i, planned) in plan.iter().enumerate() {
        // The VDB is the resume state: `var/db/pkg/<cat>/<pf>` exists iff this
        // exact version is already installed in the target root.
        let pkg_vdb = merge_root.join("var/db/pkg").join(&planned.cpv);
        if !emptytree && pkg_vdb.exists() {
            println!(
                ">>> [{}/{total}] {} is already installed — skipping",
                i + 1,
                planned.cpv
            );
            skipped += 1;
            continue;
        }

        println!("\n>>> Emerging ({} of {total}) {}", i + 1, planned.cpv);
        match ebuild::build_and_merge(
            &planned.ebuild_path,
            &planned.use_flags,
            work_base,
            merge_root,
            distdir,
            quiet,
        )
        .await
        {
            Ok(()) => merged += 1,
            Err(e) => {
                eprintln!(">>> Failed to emerge {} — {e:#}", planned.cpv);
                failures.push(MergeFailure {
                    cpv: planned.cpv.clone(),
                    log: work_base.join(&planned.cpv).join("build.log"),
                    cause: format!("{e:#}"),
                });
                if !keep_going {
                    eprintln!(">>> Stopping (pass --keep-going to continue past failures).");
                    break;
                }
            }
        }
    }

    // Refresh ${ROOT}/etc/profile.env and the linker cache, as emerge does
    // after merging — only worthwhile if something was actually installed.
    if merged > 0
        && let Err(e) = maint::env::env_update(merge_root)
    {
        eprintln!("warning: env-update failed: {e:#}");
    }

    if failures.is_empty() {
        let extra = if skipped > 0 {
            format!(" ({skipped} already installed)")
        } else {
            String::new()
        };
        println!("\n>>> Done — {merged} package(s) merged into {merge_root}{extra}");
        return Ok(());
    }

    eprintln!("\n>>> {} package(s) failed to merge:", failures.len());
    for f in &failures {
        eprintln!("  * {}", f.cpv);
        eprintln!("      {}", f.cause);
        if f.log.exists() {
            eprintln!("      log: {}", f.log);
        }
    }
    if merged > 0 || skipped > 0 {
        eprintln!(
            "    ({merged} merged, {skipped} already installed, {} failed of {total})",
            failures.len()
        );
    }
    bail!("{} of {total} package(s) failed to merge", failures.len());
}

async fn run_applet(applet: &Applet, globals: &cli::Cli) -> Result<()> {
    match applet {
        Applet::Ebuild {
            ebuild_path,
            phase,
            work_dir,
        } => {
            let repo_override = globals.repo.as_deref();
            let root = globals.root.as_deref().unwrap_or("/");
            ebuild::run(
                ebuild_path,
                phase,
                work_dir.as_deref(),
                repo_override,
                camino::Utf8Path::new(root),
            )
            .await
        }
        Applet::Maint { command } => run_maint(command, globals),
        Applet::Portageq { command, args } => {
            eprintln!("portageq: command={} args={:?}", command, args);
            bail!("not implemented: portageq")
        }
        Applet::Sync { repos } => {
            eprintln!("sync: repos={:?}", repos);
            bail!("not implemented: sync")
        }
        Applet::Depclean { atoms } => {
            let parsed = parse_atoms(atoms);
            eprintln!("depclean: atoms={:?}", parsed);
            bail!("not implemented: depclean")
        }
        Applet::Regen {
            repos,
            output,
            repos_dir,
            jobs,
            dedup,
        } => {
            regen::run(
                repos,
                &globals.repo_path(),
                repos_dir.as_deref(),
                output.clone(),
                *jobs,
                *dedup,
            )
            .await
        }
        Applet::Quickpkg { atoms } => {
            let parsed = parse_atoms(atoms);
            eprintln!("quickpkg: atoms={:?}", parsed);
            bail!("not implemented: quickpkg")
        }
        Applet::Mirror { args } => {
            eprintln!("mirror: args={:?}", args);
            bail!("not implemented: mirror")
        }
        Applet::Pkg { command } => pkg::run(command),
        Applet::Query { command } => run_query(command, globals).await,
        Applet::Clean { target } => run_clean(target),
        Applet::Use {
            add,
            remove,
            make_conf,
        } => use_flags::run(add, remove, make_conf.as_deref()),
        Applet::Revdep { args } => {
            eprintln!("revdep: args={:?}", args);
            bail!("not implemented: revdep")
        }
        Applet::Read { args } => {
            eprintln!("read: args={:?}", args);
            bail!("not implemented: read")
        }
        Applet::News { command } => run_news(command),
        Applet::Glsa { command } => run_glsa(command),
        Applet::Log { command } => run_log(command),
        Applet::Grep { pattern, paths } => {
            eprintln!("grep: pattern={} paths={:?}", pattern, paths);
            bail!("not implemented: grep")
        }
        Applet::Search {
            all,
            desc,
            name_only,
            homepage,
            pattern,
        } => {
            search::run(
                &globals.search_repos(),
                pattern.as_deref(),
                *all,
                *desc,
                *name_only,
                *homepage,
            )
            .await
        }
        Applet::Atom { atoms } => run_atom(atoms),
        Applet::Select { module, args } => {
            eprintln!("select: module={} args={:?}", module, args);
            bail!("not implemented: select")
        }
        Applet::Dispatch => {
            eprintln!("dispatch-conf");
            bail!("not implemented: dispatch-conf")
        }
        Applet::Etc => {
            eprintln!("etc-update");
            bail!("not implemented: etc-update")
        }
        Applet::Env => {
            let root = globals
                .root
                .as_deref()
                .map_or_else(|| camino::Utf8PathBuf::from("/"), camino::Utf8PathBuf::from);
            maint::env::env_update(&root)
        }
    }
}

fn run_maint(command: &Option<MaintCommand>, globals: &cli::Cli) -> Result<()> {
    match command {
        None => bail!("not implemented: emaint (no subcommand)"),
        Some(MaintCommand::All) => bail!("not implemented: emaint all"),
        Some(MaintCommand::Binhost) => bail!("not implemented: emaint binhost"),
        Some(MaintCommand::Cleanconfmem) => bail!("not implemented: emaint cleanconfmem"),
        Some(MaintCommand::Cleanresume) => bail!("not implemented: emaint cleanresume"),
        Some(MaintCommand::Logs) => bail!("not implemented: emaint logs"),
        Some(MaintCommand::Merges) => bail!("not implemented: emaint merges"),
        Some(MaintCommand::Movebin) => bail!("not implemented: emaint movebin"),
        Some(MaintCommand::Moveinst) => {
            let vdb = open_vdb(globals)?;
            let resolved = globals.repo_path();
            let repo_path = camino::Utf8Path::new(&resolved);
            maint::moveinst::run(repo_path, &vdb)
        }
        Some(MaintCommand::RegenUse { output }) => {
            let resolved = globals.repo_path();
            let repo_path = camino::Utf8Path::new(&resolved);
            maint::regen_use::run(repo_path, output.as_deref())
        }
        Some(MaintCommand::Revisions { repos }) => {
            let root = globals.root.as_deref().map(camino::Utf8Path::new);
            maint::revisions::run(repos, root)
        }
        Some(MaintCommand::Sync { repos }) => {
            eprintln!("emaint: sync repos={:?}", repos);
            bail!("not implemented: emaint sync")
        }
        Some(MaintCommand::World { fix }) => {
            let vdb = open_vdb(globals)?;
            let root = globals.root.as_deref().map(camino::Utf8Path::new);
            maint::world::run(&vdb, *fix, root)
        }
    }
}

async fn run_query(command: &QueryCommand, globals: &cli::Cli) -> Result<()> {
    match command {
        QueryCommand::Belongs { file } => {
            let vdb = open_vdb(globals)?;
            vdb::query_belongs(&vdb, file)
        }
        QueryCommand::Check { atom } => {
            let vdb = open_vdb(globals)?;
            query::check::run(&vdb, atom)
        }
        QueryCommand::Depends { atom } => {
            let vdb = open_vdb(globals).ok();
            query::depends::run(
                &std::path::PathBuf::from(globals.repo_path()),
                vdb.as_ref(),
                query::ResolveMode::Error,
                atom,
            )
        }
        QueryCommand::Depgraph {
            atom,
            format,
            autosolve_use,
        } => {
            let resolved = globals.repo_path();
            let repo_path = camino::Utf8Path::new(&resolved);
            if !repo_path.is_dir() {
                bail!("repo not found at {resolved}");
            }
            let repo = portage_repo::Repository::open(repo_path.as_std_path())?;
            let vdb = open_vdb(globals).ok();
            let parsed = query::resolve_atoms(atom, &repo, vdb.as_ref(), query::ResolveMode::Error);
            let atoms: Vec<String> = parsed.iter().map(|d| d.to_string()).collect();
            if atoms.is_empty() {
                bail!("equery depgraph: no valid atoms");
            }
            let root = globals.root.as_deref().map(camino::Utf8Path::new);
            let outcome = query::depgraph::depgraph(query::depgraph::DepgraphOpts {
                repo_path,
                atoms: &atoms,
                arch: &globals.arch,
                format: *format,
                verbose: globals.verbose,
                empty: globals.emptytree,
                autounmask: globals.autounmask,
                autounmask_write: globals.autounmask_write,
                autosolve_use: *autosolve_use || globals.autosolve_use,
                multi_repo: globals.repo.is_none(),
                root,
            })
            .await?;
            if outcome.exit_code != 0 {
                std::process::exit(outcome.exit_code);
            }
            Ok(())
        }
        QueryCommand::Files { atom } => {
            let vdb = open_vdb(globals)?;
            vdb::query_files(&vdb, atom)
        }
        QueryCommand::Has { atom } => {
            let vdb = open_vdb(globals)?;
            query::has::run(&vdb, atom)
        }
        QueryCommand::Hasuse { flag } => {
            query::hasuse::run(&std::path::PathBuf::from(globals.repo_path()), flag)
        }
        QueryCommand::Keywords { atom } => {
            let vdb = open_vdb(globals).ok();
            query::keywords::run(
                &std::path::PathBuf::from(globals.repo_path()),
                vdb.as_ref(),
                query::ResolveMode::Error,
                atom,
            )
        }
        QueryCommand::List { installed, pattern } => {
            if *installed {
                let vdb = open_vdb(globals)?;
                query::list::run_installed(&vdb, pattern)
            } else {
                query::list::run(&std::path::PathBuf::from(globals.repo_path()), pattern)
            }
        }
        QueryCommand::Meta { atom } => {
            let vdb = open_vdb(globals).ok();
            query::meta::run(
                &std::path::PathBuf::from(globals.repo_path()),
                vdb.as_ref(),
                query::ResolveMode::Error,
                atom,
            )
        }
        QueryCommand::Size { atom } => {
            let vdb = open_vdb(globals)?;
            vdb::query_size(&vdb, atom)
        }
        QueryCommand::Uses { atom } => {
            let vdb = open_vdb(globals).ok();
            query::uses::run(
                &std::path::PathBuf::from(globals.repo_path()),
                vdb.as_ref(),
                query::ResolveMode::Error,
                atom,
            )
        }
        QueryCommand::Which { atom } => {
            let vdb = open_vdb(globals).ok();
            query::which::run(
                &std::path::PathBuf::from(globals.repo_path()),
                vdb.as_ref(),
                query::ResolveMode::Error,
                atom,
            )
        }
    }
}

fn run_clean(target: &Option<CleanTarget>) -> Result<()> {
    match target {
        None => bail!("not implemented: eclean (no target)"),
        Some(CleanTarget::Dist) => bail!("not implemented: eclean dist"),
        Some(CleanTarget::Pkg) => bail!("not implemented: eclean pkg"),
    }
}

fn run_news(command: &Option<NewsCommand>) -> Result<()> {
    match command {
        None => bail!("not implemented: news (no subcommand)"),
        Some(NewsCommand::Count) => bail!("not implemented: news count"),
        Some(NewsCommand::List) => bail!("not implemented: news list"),
        Some(NewsCommand::Read { id }) => {
            eprintln!("news: read {:?}", id);
            bail!("not implemented: news read")
        }
        Some(NewsCommand::Purge) => bail!("not implemented: news purge"),
    }
}

fn run_glsa(command: &Option<GlsaCommand>) -> Result<()> {
    match command {
        None => bail!("not implemented: glsa (no subcommand)"),
        Some(GlsaCommand::List) => bail!("not implemented: glsa list"),
        Some(GlsaCommand::Check { ids }) => {
            eprintln!("glsa: check {:?}", ids);
            bail!("not implemented: glsa check")
        }
        Some(GlsaCommand::Fix { ids }) => {
            eprintln!("glsa: fix {:?}", ids);
            bail!("not implemented: glsa fix")
        }
    }
}

fn run_log(command: &Option<LogCommand>) -> Result<()> {
    match command {
        None => bail!("not implemented: log (no subcommand)"),
        Some(LogCommand::Current) => bail!("not implemented: log current"),
        Some(LogCommand::List { limit }) => {
            eprintln!("log: list limit={:?}", limit);
            bail!("not implemented: log list")
        }
        Some(LogCommand::Time { atom }) => {
            eprintln!("log: time atom={:?}", atom);
            bail!("not implemented: log time")
        }
    }
}

fn open_vdb(globals: &cli::Cli) -> Result<portage_vdb::Vdb> {
    let vdb_path = globals
        .vdb
        .as_deref()
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            globals
                .root
                .as_deref()
                .map(|r| format!("{}/var/db/pkg", r.trim_end_matches('/')))
                .unwrap_or_else(|| "/var/db/pkg".to_string())
        });
    portage_vdb::Vdb::open(vdb_path.as_str())
        .with_context(|| format!("failed to open VDB at {vdb_path}"))
}

fn run_atom(atoms: &[String]) -> Result<()> {
    for raw in atoms {
        match portage_atom::Dep::from_str(raw) {
            Ok(dep) => println!("{dep}"),
            Err(e) => eprintln!("error: '{}': {}", raw, e),
        }
    }
    Ok(())
}
