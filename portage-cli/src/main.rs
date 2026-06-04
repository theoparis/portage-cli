mod cli;
mod ebuild;
mod error;
mod maint;
mod pkg;
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
            run_emerge(&cli)
        }
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run_emerge(cli: &cli::Cli) -> Result<()> {
    let atoms = parse_atoms(&cli.atoms);
    if cli.pretend || cli.verbose {
        for atom in &atoms {
            println!("{atom}");
        }
    }
    bail!("not implemented: emerge")
}

async fn run_applet(applet: &Applet, globals: &cli::Cli) -> Result<()> {
    match applet {
        Applet::Ebuild { ebuild_path, phase, work_dir, root } => {
            let repo_override = globals.repo.as_deref();
            ebuild::run(ebuild_path, phase, work_dir.as_deref(), repo_override, root).await
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
        Applet::Regen { repos, output, repos_dir, jobs, dedup } => {
            let resolved = globals.repo_path();
            let repo_path = repos.first().map(|r| r.as_str()).unwrap_or(&resolved);
            regen::run(repo_path, repos_dir.as_deref(), output.clone(), *jobs, *dedup).await
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
        Applet::Use { add, remove, make_conf } => {
            use_flags::run(add, remove, make_conf.as_deref())
        }
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
        Applet::Search { all, desc, name_only, homepage, pattern } => {
            search::run(&globals.search_repos(), pattern.as_deref(), *all, *desc, *name_only, *homepage).await
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
            eprintln!("env-update");
            bail!("not implemented: env-update")
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
            query::depends::run(&std::path::PathBuf::from(globals.repo_path()), atom)
        }
        QueryCommand::Depgraph { atom, format } => {
            let parsed = parse_atoms(atom);
            let atoms: Vec<String> = parsed.iter().map(|d| d.to_string()).collect();
            if atoms.is_empty() {
                bail!("equery depgraph: no valid atoms");
            }
            let resolved = globals.repo_path();
            let repo_path = camino::Utf8Path::new(&resolved);
            if !repo_path.is_dir() {
                bail!("repo not found at {resolved}");
            }
            query::depgraph::depgraph(repo_path, &atoms, &globals.arch, *format, globals.verbose).await
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
            query::keywords::run(&std::path::PathBuf::from(globals.repo_path()), atom)
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
            query::meta::run(&std::path::PathBuf::from(globals.repo_path()), vdb.as_ref(), atom)
        }
        QueryCommand::Size { atom } => {
            let vdb = open_vdb(globals)?;
            vdb::query_size(&vdb, atom)
        }
        QueryCommand::Uses { atom } => {
            let vdb = open_vdb(globals).ok();
            query::uses::run(&std::path::PathBuf::from(globals.repo_path()), vdb.as_ref(), atom)
        }
        QueryCommand::Which { atom } => {
            query::which::run(&std::path::PathBuf::from(globals.repo_path()), atom)
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
