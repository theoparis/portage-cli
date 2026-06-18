mod bdepend_avail;
mod cli;
mod ebuild;
mod error;
mod maint;
mod pkg;
mod postprocess;
mod preflight;
mod query;
mod regen;
mod search;
mod setup;
mod use_flags;
mod vdb;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::collections::VecDeque;
use std::str::FromStr;

use anyhow::{Context, bail};
use camino::Utf8Path;
use clap::Parser;
use error::Result;
use futures_util::stream::{FuturesUnordered, StreamExt};

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

/// Expand `@set` references in `raw` to concrete atoms, leaving plain atoms
/// untouched. Sets are a portage-config concept (not PMS); resolution lives in
/// `portage_repo::SetResolver`. The profile stack comes from
/// `<config_root>/etc/portage/make.profile` (for `@system`/`@profile`); user
/// sets, `@world`, and `@selected` are read from `eroot` (the install target).
///
/// Failures (unknown set, bad profile link) are reported and the offending
/// token dropped, matching `parse_atoms`' tolerance of bad atoms — a typo
/// shouldn't abort the whole run, and `@system` against a host with no profile
/// is a configuration error, not a crash.
fn expand_sets(raw: &[String], config_root: Option<&Utf8Path>, eroot: &Utf8Path) -> Vec<String> {
    // Build the resolver lazily, only when a set ref is actually present, so a
    // plain `em foo` (no sets) pays no profile-build cost.
    #[allow(unused_assignments)] // stack_holder's initial None is overwritten before read.
    let mut stack_holder: Option<portage_repo::ProfileStack> = None;
    let mut resolver: Option<portage_repo::SetResolver<'_>> = None;
    let mut out = Vec::with_capacity(raw.len());
    for s in raw {
        let Some(name) = portage_repo::set_name(s) else {
            out.push(s.clone());
            continue;
        };
        if resolver.is_none() {
            let portage_dir = config_root
                .unwrap_or(Utf8Path::new("/"))
                .join("etc/portage");
            let profile_link = portage_dir.join("make.profile");
            match std::fs::canonicalize(profile_link.as_std_path())
                .map_err(|e| anyhow::anyhow!("cannot resolve make.profile for @set expansion: {e}"))
                .and_then(|p| {
                    portage_repo::ProfileStack::build(p)
                        .map_err(|e| anyhow::anyhow!("failed to build profile stack: {e}"))
                }) {
                Ok(st) => {
                    stack_holder = Some(st);
                    // Safe: stack_holder outlives resolver (both dropped at fn end).
                    let stack = stack_holder.as_ref().unwrap();
                    resolver = Some(portage_repo::SetResolver::new(stack, eroot));
                }
                Err(e) => {
                    eprintln!("warning: cannot expand @{name}: {e}");
                    continue;
                }
            }
        }
        match resolver.as_ref().unwrap().resolve(name) {
            Ok(atoms) => out.extend(atoms.iter().map(|d| d.to_string())),
            Err(e) => eprintln!("warning: skipping @{name}: {e}"),
        }
    }
    out
}

#[tokio::main]
async fn main() {
    // Portage's ebuild.sh sets `umask 022` before running any phase; mirror it
    // so file and directory modes under ${D} and the build tree match a real
    // merge regardless of the invoking shell's umask. The install helpers
    // additionally chmod each created image dir to 0755 (see mkdir_p_mode), so
    // they stay correct even under a tighter ebuild-local umask; this call
    // covers everything else (ebuild-written files, distfiles, the prefix
    // layout, cache regen).
    rustix::process::umask(rustix::fs::Mode::from_bits_truncate(0o022));

    let cli = cli::Cli::parse();
    cli.color.write_global();

    // --setup bootstraps the prefix layout before anything else. With no atoms
    // or applet it's a standalone "prepare this prefix" command.
    if cli.setup {
        if let Err(e) = setup::bootstrap(&cli.roots()) {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        if cli.applet.is_none() && cli.atoms.is_empty() {
            return;
        }
    }

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
    // Root model (docs/root-model.md): config from roots.config (host for a
    // --prefix overlay), installed view = VDB(base) ∪ VDB(target), and the
    // plan installs into target.
    let roots = cli.roots();
    // Expand @set references (e.g. @system, @world) to concrete atoms before
    // resolution. Sets are read from the config root's profile (@system) and
    // the merge target (@world/@selected, user sets).
    let expanded = expand_sets(&cli.atoms, roots.config(), roots.merge_root());
    let parsed = query::resolve_atoms(&expanded, &repo, vdb.as_ref(), mode);
    let atoms: Vec<String> = parsed.iter().map(|d| d.to_string()).collect();
    if atoms.is_empty() {
        bail!("em: no valid atoms");
    }
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
        autounmask_write: cli.autounmask_write,
        autosolve_use: cli.autosolve_use,
        multi_repo: cli.repo.is_none(),
        roots: &roots,
        onlydeps: cli.onlydeps,
        with_bdeps: cli.with_bdeps,
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
    // --prefix additionally relocates distfiles and the build trees under the
    // target (a self-contained tree); --root leaves them at the host defaults.
    let relocate = roots.relocate().then(|| roots.merge_root());
    let distdir = relocate.map(|p| p.join("var/cache/distfiles"));
    let work_base = ebuild::default_work_base(relocate);

    if outcome.plan.is_empty() {
        return Ok(());
    }

    // Pre-flight: fail fast with a clear message if any plan entry's build
    // dependencies won't be present when it builds, rather than mid-build.
    preflight::check(&outcome.plan, &roots)?;

    if cli.ask && !confirm_merge(outcome.plan.len())? {
        println!(">>> Quitting.");
        return Ok(());
    }

    run_merge_plan(
        &outcome.plan,
        &outcome.build_blockers,
        &roots,
        &work_base,
        distdir.as_deref(),
        cli.quiet,
        cli.keep_going,
        cli.emptytree,
        cli.jobs.map(|j| j as usize).unwrap_or(1).max(1),
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
#[allow(clippy::too_many_arguments)]
async fn run_merge_plan(
    plan: &[query::depgraph::PlannedMerge],
    blockers: &[Vec<usize>],
    roots: &cli::Roots,
    work_base: &camino::Utf8Path,
    distdir: Option<&camino::Utf8Path>,
    quiet: bool,
    keep_going: bool,
    emptytree: bool,
    jobs: usize,
) -> Result<()> {
    let merge_root = roots.merge_root();
    let total = plan.len();

    let (merged, skipped, failures) = if jobs <= 1 {
        merge_sequential(
            plan, roots, work_base, distdir, quiet, keep_going, emptytree,
        )
        .await
    } else {
        merge_parallel(
            plan, blockers, roots, work_base, distdir, quiet, keep_going, emptytree, jobs,
        )
        .await
    };

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

/// Sequential build+merge in install order (the `--jobs 1` / default path).
/// Returns `(merged, skipped, failures)`.
async fn merge_sequential(
    plan: &[query::depgraph::PlannedMerge],
    roots: &cli::Roots,
    work_base: &camino::Utf8Path,
    distdir: Option<&camino::Utf8Path>,
    quiet: bool,
    keep_going: bool,
    emptytree: bool,
) -> (usize, usize, Vec<MergeFailure>) {
    let merge_root = roots.merge_root();
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
            roots.config(),
            roots.build_sysroot(),
            roots.eprefix(),
            None,
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
    (merged, skipped, failures)
}

/// Tracks which plan entries are ready to build given the build-dep `blockers`
/// (each entry's in-plan predecessors). A node is ready once all its blockers
/// have `complete`d; this is the topological bookkeeping behind `--jobs`,
/// independent of how many run at once or in what real-time order they finish.
struct Scheduler {
    /// Remaining un-completed blockers per node.
    outstanding: Vec<usize>,
    /// Reverse adjacency: `dependents[j]` are nodes blocked on `j`.
    dependents: Vec<Vec<usize>>,
    /// Nodes with no outstanding blockers, awaiting a build slot.
    ready: VecDeque<usize>,
}

impl Scheduler {
    fn new(blockers: &[Vec<usize>]) -> Self {
        let outstanding: Vec<usize> = blockers.iter().map(Vec::len).collect();
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); blockers.len()];
        for (i, bs) in blockers.iter().enumerate() {
            for &j in bs {
                dependents[j].push(i);
            }
        }
        let ready = (0..blockers.len())
            .filter(|&i| outstanding[i] == 0)
            .collect();
        Scheduler {
            outstanding,
            dependents,
            ready,
        }
    }

    /// Pop the next node whose blockers are all satisfied, if any is waiting.
    fn next_ready(&mut self) -> Option<usize> {
        self.ready.pop_front()
    }

    /// Mark node `i` finished (built or skipped), unblocking its dependents.
    fn complete(&mut self, i: usize) {
        for d in std::mem::take(&mut self.dependents[i]) {
            self.outstanding[d] -= 1;
            if self.outstanding[d] == 0 {
                self.ready.push_back(d);
            }
        }
    }
}

/// Parallel build+merge for `--jobs N > 1`. Up to `jobs` packages *build*
/// concurrently; each only starts once its in-plan build dependencies
/// (`blockers`) have completed, so build order is respected. The compile phases
/// run in parallel (the heavy work is in child processes we await), while the
/// merge critical section is serialised by a shared async lock — so the live
/// root, VDB counter, and world/profile files are only mutated by one package
/// at a time. Returns `(merged, skipped, failures)`.
///
/// Concurrency is single-threaded (`FuturesUnordered`, not spawned tasks): the
/// `EbuildShell` need not be `Send`, and parallelism still comes from the
/// concurrently-running build subprocesses.
#[allow(clippy::too_many_arguments)]
async fn merge_parallel(
    plan: &[query::depgraph::PlannedMerge],
    blockers: &[Vec<usize>],
    roots: &cli::Roots,
    work_base: &camino::Utf8Path,
    distdir: Option<&camino::Utf8Path>,
    quiet: bool,
    keep_going: bool,
    emptytree: bool,
    jobs: usize,
) -> (usize, usize, Vec<MergeFailure>) {
    let merge_root = roots.merge_root();
    let total = plan.len();
    let merge_gate = tokio::sync::Mutex::new(());

    let mut sched = Scheduler::new(blockers);
    let mut merged = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<MergeFailure> = Vec::new();
    let mut started = 0usize;
    let mut stop_new = false;
    let mut inflight = FuturesUnordered::new();

    loop {
        while !stop_new && inflight.len() < jobs {
            let Some(i) = sched.next_ready() else { break };
            let planned = &plan[i];
            if !emptytree && merge_root.join("var/db/pkg").join(&planned.cpv).exists() {
                println!(">>> {} is already installed — skipping", planned.cpv);
                skipped += 1;
                sched.complete(i);
                continue;
            }
            started += 1;
            println!(
                ">>> Emerging ({started} of {total}) {} [+{} building]",
                planned.cpv,
                inflight.len()
            );
            let gate = &merge_gate;
            inflight.push(async move {
                let res = ebuild::build_and_merge(
                    &planned.ebuild_path,
                    &planned.use_flags,
                    work_base,
                    merge_root,
                    distdir,
                    quiet,
                    roots.config(),
                    roots.build_sysroot(),
                    roots.eprefix(),
                    Some(gate),
                )
                .await;
                (i, res)
            });
        }

        let Some((i, res)) = inflight.next().await else {
            break;
        };
        match res {
            Ok(()) => {
                merged += 1;
                sched.complete(i);
            }
            Err(e) => {
                eprintln!(">>> Failed to emerge {} — {e:#}", plan[i].cpv);
                failures.push(MergeFailure {
                    cpv: plan[i].cpv.clone(),
                    log: work_base.join(&plan[i].cpv).join("build.log"),
                    cause: format!("{e:#}"),
                });
                // Dependents stay blocked (their count never reaches 0), so a
                // package whose build dep failed is never started.
                if !keep_going {
                    stop_new = true;
                    eprintln!(
                        ">>> Stopping new builds (pass --keep-going to continue past failures)."
                    );
                }
            }
        }
    }
    (merged, skipped, failures)
}

async fn run_applet(applet: &Applet, globals: &cli::Cli) -> Result<()> {
    match applet {
        // Internal helper shim entry point: run the helper and exit with its
        // status (the shim's caller — `find -exec`/`xargs` — checks it).
        Applet::Helper { name, args } => {
            std::process::exit(portage_repo::run_helper(name, args).await);
        }
        Applet::Ebuild {
            ebuild_path,
            phase,
            work_dir,
        } => {
            let repo_override = globals.repo.as_deref();
            let roots = globals.roots();
            ebuild::run(
                ebuild_path,
                phase,
                work_dir.as_deref(),
                repo_override,
                roots.merge_root(),
                roots.config(),
                roots.build_sysroot(),
                roots.eprefix(),
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
        Applet::Env => maint::env::env_update(globals.roots().merge_root()),
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
            let roots = globals.roots();
            maint::revisions::run(repos, roots.target())
        }
        Some(MaintCommand::Sync { repos }) => {
            eprintln!("emaint: sync repos={:?}", repos);
            bail!("not implemented: emaint sync")
        }
        Some(MaintCommand::World { fix }) => {
            let vdb = open_vdb(globals)?;
            let roots = globals.roots();
            maint::world::run(&vdb, *fix, roots.target())
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
            let roots = globals.roots();
            let outcome = query::depgraph::depgraph(query::depgraph::DepgraphOpts {
                repo_path,
                atoms: &atoms,
                arch: &globals.arch,
                format: *format,
                verbose: globals.verbose,
                empty: globals.emptytree,
                autounmask_write: globals.autounmask_write,
                autosolve_use: *autosolve_use || globals.autosolve_use,
                multi_repo: globals.repo.is_none(),
                roots: &roots,
                onlydeps: globals.onlydeps,
                with_bdeps: globals.with_bdeps,
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
            format!(
                "{}/var/db/pkg",
                globals.roots().merge_root().as_str().trim_end_matches('/')
            )
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

#[cfg(test)]
mod scheduler_tests {
    use super::Scheduler;

    /// Drain the scheduler completing nodes in FIFO order, recording the order
    /// in which they were released for building. Mirrors what `merge_parallel`
    /// does, minus the actual builds.
    fn drain(blockers: &[Vec<usize>]) -> Vec<usize> {
        let mut sched = Scheduler::new(blockers);
        let mut order = Vec::new();
        while let Some(i) = sched.next_ready() {
            order.push(i);
            sched.complete(i);
        }
        order
    }

    /// Every node must appear after all of its blockers.
    fn assert_respects(blockers: &[Vec<usize>], order: &[usize]) {
        assert_eq!(
            order.len(),
            blockers.len(),
            "every node scheduled exactly once"
        );
        let pos: std::collections::HashMap<usize, usize> =
            order.iter().enumerate().map(|(p, &n)| (n, p)).collect();
        for (node, bs) in blockers.iter().enumerate() {
            for &b in bs {
                assert!(pos[&b] < pos[&node], "blocker {b} must precede {node}");
            }
        }
    }

    #[test]
    fn independent_nodes_are_all_ready_immediately() {
        let blockers = vec![vec![], vec![], vec![]];
        let order = drain(&blockers);
        assert_eq!(order, [0, 1, 2]);
    }

    #[test]
    fn a_chain_serialises() {
        // 0 <- 1 <- 2 (2 depends on 1 depends on 0).
        let blockers = vec![vec![], vec![0], vec![1]];
        let order = drain(&blockers);
        assert_eq!(order, [0, 1, 2]);
        assert_respects(&blockers, &order);
    }

    #[test]
    fn a_diamond_respects_both_paths() {
        // 0 <- {1,2} <- 3.
        let blockers = vec![vec![], vec![0], vec![0], vec![1, 2]];
        let order = drain(&blockers);
        assert_respects(&blockers, &order);
        assert_eq!(*order.last().unwrap(), 3, "the join builds last");
    }

    #[test]
    fn a_node_with_two_blockers_waits_for_the_later_one() {
        // 2 depends on both 0 and 1; it must not be ready until both complete.
        let blockers = vec![vec![], vec![], vec![0, 1]];
        let mut sched = Scheduler::new(&blockers);
        assert_eq!(sched.next_ready(), Some(0));
        assert_eq!(sched.next_ready(), Some(1));
        assert_eq!(sched.next_ready(), None, "2 blocked until both deps done");
        sched.complete(0);
        assert_eq!(sched.next_ready(), None, "still blocked on 1");
        sched.complete(1);
        assert_eq!(sched.next_ready(), Some(2));
    }

    #[test]
    fn a_failed_blocker_strands_its_dependents() {
        // If 0 fails (never `complete`d), 1 and the transitively-blocked 2 are
        // never released — matching merge_parallel skipping a failed dep's tree.
        let blockers = vec![vec![], vec![0], vec![1]];
        let mut sched = Scheduler::new(&blockers);
        assert_eq!(sched.next_ready(), Some(0));
        // 0 "fails": we do not call complete(0).
        assert_eq!(sched.next_ready(), None);
        assert_eq!(sched.next_ready(), None);
    }
}
