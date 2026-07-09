//! Parallel and sequential merge scheduling.

use std::collections::VecDeque;

use anyhow::{Context, bail};
use futures_util::stream::{FuturesUnordered, StreamExt};

use crate::binpkg;
use crate::cli;
use crate::ebuild;
use crate::error::Result;
use crate::maint;
use crate::query;

/// One package's merge failure, for the end-of-run report.
struct MergeFailure {
    cpv: String,
    log: camino::Utf8PathBuf,
    cause: String,
}

/// Verify `pkgdir` can actually be written to (creating it if missing) — the
/// `--buildpkg` preflight in [`run_merge_plan`]. A probe file is written and
/// removed rather than just checking metadata, since permission bits alone
/// don't capture every reason a write can fail (e.g. a read-only mount).
fn check_pkgdir_writable(pkgdir: &camino::Utf8Path) -> Result<()> {
    std::fs::create_dir_all(pkgdir.as_std_path()).with_context(|| format!("creating {pkgdir}"))?;
    let probe = pkgdir.join(".em-write-probe");
    std::fs::write(probe.as_std_path(), b"").with_context(|| format!("writing to {pkgdir}"))?;
    let _ = std::fs::remove_file(probe.as_std_path());
    Ok(())
}

/// Prompt before merging (`--ask`). Defaults to no on empty input or EOF.
pub(crate) fn confirm_merge(count: usize) -> Result<bool> {
    use std::io::Write;
    print!("\n>>> Would you like to merge these {count} package(s)? [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Ok(false);
    }
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}

/// Which [`cli::Roots`] a plan entry actually installs into: the outer EROOT
/// (`host_roots`) for a Host-rooted entry — an unsatisfied BDEPEND scheduled
/// onto the build host by a `--target` solve (see `cross_target_runtime_deps`
/// in portage-atom-pubgrub) — or the `--target`-substituted sysroot (`roots`,
/// the resolved install target) for everything else. `host_roots` equals
/// `roots` outside `--target`, so this is a no-op there.
///
/// Found live: the merge loop used a single, plan-wide root for every entry
/// regardless of `PlannedMerge.merge_root`, so a Host BDEPEND (e.g.
/// `dev-python/jinja2`, rebuilt for a python target the real host lacked)
/// silently built into the sysroot instead — the package "succeeded" but
/// never became available where the later build that needed it actually
/// looked. See `todo/stage-build-shakeout.md`.
fn entry_roots<'a>(
    planned: &query::depgraph::PlannedMerge,
    roots: &'a cli::Roots,
    host_roots: &'a cli::Roots,
) -> &'a cli::Roots {
    if planned.merge_root == query::depgraph::MergeRoot::Host {
        host_roots
    } else {
        roots
    }
}

/// Build and merge a resolved plan in install order.
///
/// Resume comes for free from the target VDB: a package already recorded
/// there at the planned version is skipped (a previous run merged it), so
/// re-running after an interruption continues from the first unmerged entry
/// without a separate state file. `--emptytree` forces every entry to rebuild.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_merge_plan(
    plan: &[query::depgraph::PlannedMerge],
    blockers: &[Vec<usize>],
    roots: &cli::Roots,
    work_base: &camino::Utf8Path,
    distdir: Option<&camino::Utf8Path>,
    quiet: bool,
    keep_going: bool,
    emptytree: bool,
    jobs: usize,
    buildpkg: bool,
    usepkg: bool,
    getbinpkg: bool,
    getbinpkgonly: bool,
    globals: &cli::Cli,
) -> Result<()> {
    let merge_root = roots.merge_root();
    let total = plan.len();

    // Fail fast: verify PKGDIR is actually writable *before* starting a
    // potentially multi-hour build, rather than discovering it deep into a
    // `--keep-going` run once dozens of packages have already silently died.
    // Found live (todo/stage-build-shakeout.md): a stage3 --buildpkg attempt
    // hit a permission-denied PKGDIR (fixed separately — resolve_pkgdir is now
    // root-aware), and each failure surfaced as an unexplained, silent worker
    // death rather than the single clear error this check now gives instead.
    if buildpkg {
        let pkgdir = binpkg::resolve_pkgdir(globals);
        check_pkgdir_writable(&pkgdir)
            .with_context(|| format!("--buildpkg: PKGDIR {pkgdir} is not writable"))?;
    }

    // Implication chain (portage actions.py): -g ⇒ --usepkg, -G ⇒ --getbinpkg +
    // binpkg-only (no source). So both enable local reuse; local overrides remote.
    let want_local = usepkg || getbinpkg || getbinpkgonly;
    let want_remote = getbinpkg || getbinpkgonly;
    let enforce_no_source = getbinpkgonly;

    // Open the local binpkg index once if any binpkg reuse is in effect.
    let binpkg_index = if want_local {
        let pkgdir = binpkg::resolve_pkgdir(globals);
        match binpkg::BinpkgIndex::open(pkgdir.as_std_path()) {
            Ok(idx) => {
                if idx.len() > 0 {
                    println!(
                        ">>> --usepkg: {} local binary package(s) in {pkgdir}",
                        idx.len()
                    );
                }
                Some(idx)
            }
            Err(e) => {
                eprintln!("warning: --usepkg index unavailable ({pkgdir}): {e:#}");
                None
            }
        }
    } else {
        None
    };

    // Fetch each configured remote binhost's Packages index. `-g`/`-G` only.
    let remote_indices: Vec<binpkg::RemoteBinpkgIndex> = if want_remote {
        let binhosts = binpkg::portage_binhosts(globals);
        if binhosts.is_empty() {
            eprintln!(
                "warning: --getbinpkg set but no binhost configured (PORTAGE_BINHOST unset, no binrepos.conf)"
            );
        }
        let mut fetched = Vec::new();
        for base in &binhosts {
            match portage_distfiles::fetch_index(base).await {
                Ok(text) => {
                    let idx = binpkg::RemoteBinpkgIndex::new(&text, base);
                    println!(">>> --getbinpkg: {} package(s) on {base}", idx.len());
                    fetched.push(idx);
                }
                Err(e) => {
                    eprintln!("warning: could not fetch binhost index {base}: {e:#}");
                }
            }
        }
        fetched
    } else {
        Vec::new()
    };

    // A `--target` plan can carry `MergeRoot::Host` entries (an unsatisfied
    // BDEPEND scheduled onto the build host — see `cross_target_runtime_deps`
    // in portage-atom-pubgrub). `roots` here is the `--target`-substituted
    // sysroot; `broot()` is where a Host entry actually belongs — the real
    // host `/` for plain `--root` (portage `ROOT=` parity: BDEPEND resolves
    // and installs on the host, full stop), matching `base_roots()` for
    // `--prefix`/`--local`. NOT `base_roots()` directly: that's "the outer
    // EROOT" (where crossdev's own `cross-*` toolchain *bootstrap* packages
    // land via the separate `bypass_cross_root` mechanism in `emerge.rs`) —
    // a different, unprivileged-writable-location concern from "where does
    // an ordinary package's BDEPEND resolve". Equal to `roots` when `--target`
    // isn't active, so this is a no-op outside cross builds.
    let host_roots = globals.broot();
    let (merged, skipped, failures) = if jobs <= 1 {
        merge_sequential(
            plan,
            roots,
            &host_roots,
            work_base,
            distdir,
            quiet,
            keep_going,
            emptytree,
            buildpkg,
            binpkg_index.as_ref(),
            &remote_indices,
            enforce_no_source,
        )
        .await
    } else {
        merge_parallel(
            plan,
            blockers,
            roots,
            &host_roots,
            work_base,
            distdir,
            quiet,
            keep_going,
            emptytree,
            jobs,
            buildpkg,
            binpkg_index.as_ref(),
            &remote_indices,
            enforce_no_source,
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
#[allow(clippy::too_many_arguments)]
async fn merge_sequential(
    plan: &[query::depgraph::PlannedMerge],
    roots: &cli::Roots,
    host_roots: &cli::Roots,
    work_base: &camino::Utf8Path,
    distdir: Option<&camino::Utf8Path>,
    quiet: bool,
    keep_going: bool,
    emptytree: bool,
    buildpkg: bool,
    binpkg_index: Option<&binpkg::BinpkgIndex>,
    remote_indices: &[binpkg::RemoteBinpkgIndex],
    enforce_no_source: bool,
) -> (usize, usize, Vec<MergeFailure>) {
    let total = plan.len();
    let mut merged = 0usize;
    let mut skipped = 0usize;
    let mut failures: Vec<MergeFailure> = Vec::new();

    for (i, planned) in plan.iter().enumerate() {
        let entry_roots = entry_roots(planned, roots, host_roots);
        let merge_root = entry_roots.merge_root();
        // The VDB is the resume state: `var/db/pkg/<cat>/<pf>` exists iff this
        // exact version is already installed in the target root. An intentional
        // reinstall (explicit target / USE rebuild) is built anyway — emerge
        // reinstalls a requested atom by default.
        let pkg_vdb = merge_root.join("var/db/pkg").join(planned.cpv.to_string());
        if !emptytree && !planned.reinstall && pkg_vdb.exists() {
            println!(
                ">>> [{}/{total}] {} is already installed — skipping",
                i + 1,
                planned.cpv
            );
            skipped += 1;
            continue;
        }

        let desired_use: Vec<String> = planned
            .use_flags
            .iter()
            .map(|f| f.as_str().to_string())
            .collect();

        // 1. Local binpkg reuse (`-k`, or `-g`/`-G` where local overrides remote).
        let reused =
            binpkg_index.and_then(|idx| idx.find_reusable(&planned.cpv.to_string(), &desired_use));

        // 2. Remote binpkg (`-g`/`-G`): download the first matching binpkg into
        //    a per-run cache, then merge it. Local already took precedence.
        let remote_url = reused
            .is_none()
            .then(|| {
                remote_indices
                    .iter()
                    .find_map(|idx| idx.find_reusable(&planned.cpv.to_string(), &desired_use))
            })
            .flatten();

        println!("\n>>> Emerging ({} of {total}) {}", i + 1, planned.cpv);
        let result = if let Some(binpkg_path) = reused {
            println!(">>> Using binary package: {}", binpkg_path.display());
            ebuild::merge_binpkg(
                camino::Utf8Path::from_path(binpkg_path.as_path())
                    .unwrap_or_else(|| camino::Utf8Path::new("/invalid-binpkg-path")),
                &planned.ebuild_path,
                &planned.cpv,
                &planned.use_flags,
                work_base,
                merge_root,
                quiet,
                entry_roots.config(),
                entry_roots.build_sysroot(),
                entry_roots.eprefix(),
                None,
            )
            .await
        } else if let Some(url) = remote_url {
            match fetch_remote_binpkg(&url, work_base).await {
                Ok(path) => {
                    println!(">>> Fetched binary package: {url}");
                    ebuild::merge_binpkg(
                        &path,
                        &planned.ebuild_path,
                        &planned.cpv,
                        &planned.use_flags,
                        work_base,
                        merge_root,
                        quiet,
                        entry_roots.config(),
                        entry_roots.build_sysroot(),
                        entry_roots.eprefix(),
                        None,
                    )
                    .await
                }
                Err(e) => {
                    eprintln!(">>> Failed to fetch binpkg {url} — {e:#}");
                    if enforce_no_source {
                        failures.push(MergeFailure {
                            cpv: planned.cpv.to_string(),
                            log: work_base.join(planned.cpv.to_string()).join("build.log"),
                            cause: format!("remote binpkg fetch failed: {e:#}"),
                        });
                        if !keep_going {
                            break;
                        }
                        continue;
                    }
                    // Fall through to a source build.
                    ebuild::build_and_merge(
                        &planned.ebuild_path,
                        &planned.cpv,
                        &planned.use_flags,
                        work_base,
                        merge_root,
                        distdir,
                        quiet,
                        entry_roots.config(),
                        entry_roots.build_sysroot(),
                        entry_roots.eprefix(),
                        None,
                        buildpkg,
                    )
                    .await
                }
            }
        } else if enforce_no_source {
            eprintln!(
                ">>> No binary package for {} (local or remote) and --getbinpkgonly is set",
                planned.cpv
            );
            failures.push(MergeFailure {
                cpv: planned.cpv.to_string(),
                log: work_base.join(planned.cpv.to_string()).join("build.log"),
                cause: "no matching binpkg and source builds disabled (--getbinpkgonly)".into(),
            });
            if !keep_going {
                break;
            }
            continue;
        } else {
            ebuild::build_and_merge(
                &planned.ebuild_path,
                &planned.cpv,
                &planned.use_flags,
                work_base,
                merge_root,
                distdir,
                quiet,
                entry_roots.config(),
                entry_roots.build_sysroot(),
                entry_roots.eprefix(),
                None,
                buildpkg,
            )
            .await
        };
        match result {
            Ok(()) => merged += 1,
            Err(e) => {
                eprintln!(">>> Failed to emerge {} — {e:#}", planned.cpv);
                failures.push(MergeFailure {
                    cpv: planned.cpv.to_string(),
                    log: work_base.join(planned.cpv.to_string()).join("build.log"),
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

/// Download a remote binpkg from `url` into a per-run cache under `work_base`,
/// returning the local path. Cached per filename so a retry doesn't re-download.
async fn fetch_remote_binpkg(
    url: &str,
    work_base: &camino::Utf8Path,
) -> Result<camino::Utf8PathBuf> {
    let cache_dir = work_base.join("binpkg-cache");
    tokio::fs::create_dir_all(cache_dir.as_std_path())
        .await
        .with_context(|| format!("creating {cache_dir}"))?;
    // Filename: the last path segment of the URL (e.g. foo-1.0-1.gpkg.tar).
    let name = url.rsplit('/').next().unwrap_or("binpkg.gpkg.tar");
    let dest = cache_dir.join(name);
    if !dest.exists() {
        portage_distfiles::fetch_binpkg(url, dest.as_std_path())
            .await
            .with_context(|| format!("downloading {url}"))?;
    }
    Ok(dest)
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
    host_roots: &cli::Roots,
    work_base: &camino::Utf8Path,
    distdir: Option<&camino::Utf8Path>,
    quiet: bool,
    keep_going: bool,
    emptytree: bool,
    jobs: usize,
    buildpkg: bool,
    binpkg_index: Option<&binpkg::BinpkgIndex>,
    remote_indices: &[binpkg::RemoteBinpkgIndex],
    enforce_no_source: bool,
) -> (usize, usize, Vec<MergeFailure>) {
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
            let entry_roots = entry_roots(planned, roots, host_roots);
            let merge_root = entry_roots.merge_root();
            if !emptytree
                && !planned.reinstall
                && merge_root
                    .join("var/db/pkg")
                    .join(planned.cpv.to_string())
                    .exists()
            {
                println!(">>> {} is already installed — skipping", planned.cpv);
                skipped += 1;
                sched.complete(i);
                continue;
            }
            started += 1;
            let desired_use: Vec<String> = planned
                .use_flags
                .iter()
                .map(|f| f.as_str().to_string())
                .collect();
            let reused = binpkg_index
                .and_then(|idx| idx.find_reusable(&planned.cpv.to_string(), &desired_use));
            // Local overrides remote; only look remote when no local match.
            let remote_url = reused
                .is_none()
                .then(|| {
                    remote_indices
                        .iter()
                        .find_map(|idx| idx.find_reusable(&planned.cpv.to_string(), &desired_use))
                })
                .flatten();
            let tag = if reused.is_some() {
                " (binary)"
            } else if remote_url.is_some() {
                " (binary, remote)"
            } else if enforce_no_source {
                " (no binpkg — blocked by --getbinpkgonly)"
            } else {
                ""
            };
            println!(
                ">>> Emerging ({started} of {total}) {} [+{} building]{tag}",
                planned.cpv,
                inflight.len()
            );
            if let Some(p) = reused.as_ref() {
                println!(">>> Using binary package: {}", p.display());
            } else if let Some(u) = remote_url.as_ref() {
                println!(">>> Fetched binary package: {u}");
            }
            let gate = &merge_gate;
            inflight.push(async move {
                let res = if let Some(binpkg_path) = reused {
                    let binpkg_path = camino::Utf8Path::from_path(binpkg_path.as_path())
                        .unwrap_or_else(|| camino::Utf8Path::new("/invalid-binpkg-path"));
                    ebuild::merge_binpkg(
                        binpkg_path,
                        &planned.ebuild_path,
                        &planned.cpv,
                        &planned.use_flags,
                        work_base,
                        merge_root,
                        quiet,
                        entry_roots.config(),
                        entry_roots.build_sysroot(),
                        entry_roots.eprefix(),
                        Some(gate),
                    )
                    .await
                } else if let Some(url) = remote_url {
                    match fetch_remote_binpkg(&url, work_base).await {
                        Ok(path) => {
                            ebuild::merge_binpkg(
                                &path,
                                &planned.ebuild_path,
                                &planned.cpv,
                                &planned.use_flags,
                                work_base,
                                merge_root,
                                quiet,
                                entry_roots.config(),
                                entry_roots.build_sysroot(),
                                entry_roots.eprefix(),
                                Some(gate),
                            )
                            .await
                        }
                        Err(e) => Err(e),
                    }
                } else if enforce_no_source {
                    Err(anyhow::anyhow!(
                        "no matching binpkg and source builds disabled (--getbinpkgonly)"
                    ))
                } else {
                    ebuild::build_and_merge(
                        &planned.ebuild_path,
                        &planned.cpv,
                        &planned.use_flags,
                        work_base,
                        merge_root,
                        distdir,
                        quiet,
                        entry_roots.config(),
                        entry_roots.build_sysroot(),
                        entry_roots.eprefix(),
                        Some(gate),
                        buildpkg,
                    )
                    .await
                };
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
                    cpv: plan[i].cpv.to_string(),
                    log: work_base.join(plan[i].cpv.to_string()).join("build.log"),
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

#[cfg(test)]
mod entry_roots_tests {
    use super::*;
    use query::depgraph::{MergeRoot, PlannedMerge};

    fn planned(merge_root: MergeRoot) -> Result<PlannedMerge> {
        Ok(PlannedMerge {
            merge_root,
            cpv: portage_atom::Cpv::parse("dev-python/jinja2-3.1.6")?,
            ebuild_path: camino::Utf8PathBuf::new(),
            use_flags: Vec::new(),
            depend: Vec::new(),
            bdepend: Vec::new(),
            reinstall: false,
        })
    }

    #[test]
    fn host_entry_installs_into_outer_eroot_not_the_cross_sysroot() -> Result<()> {
        let roots = cli::Roots::for_test("/var/tmp/cross-stage1/usr/riscv64-unknown-linux-gnu");
        let host_roots = cli::Roots::for_test("/var/tmp/cross-stage1");
        let p = planned(MergeRoot::Host)?;
        assert_eq!(
            entry_roots(&p, &roots, &host_roots).merge_root().as_str(),
            "/var/tmp/cross-stage1"
        );
        Ok(())
    }

    #[test]
    fn target_entry_uses_the_plans_own_root() -> Result<()> {
        let roots = cli::Roots::for_test("/var/tmp/cross-stage1/usr/riscv64-unknown-linux-gnu");
        let host_roots = cli::Roots::for_test("/var/tmp/cross-stage1");
        let p = planned(MergeRoot::Target)?;
        assert_eq!(
            entry_roots(&p, &roots, &host_roots).merge_root().as_str(),
            "/var/tmp/cross-stage1/usr/riscv64-unknown-linux-gnu"
        );
        Ok(())
    }

    /// `--prefix`: an unsatisfied `MergeRoot::Host` entry must merge into
    /// the prefix, not the real host — an unprivileged overlay can't write
    /// `/`. `host_roots` here is `Cli::broot()`'s output for `--prefix`,
    /// which now resolves to the prefix (`outer_roots()`), not the host.
    #[test]
    fn host_entry_installs_into_the_prefix_under_overlay_not_the_host() -> Result<()> {
        let roots = cli::Roots::for_test("/opt/p");
        let host_roots = cli::Roots::for_test_overlay("/", "/opt/p");
        let p = planned(MergeRoot::Host)?;
        assert_eq!(
            entry_roots(&p, &roots, &host_roots).merge_root().as_str(),
            "/opt/p",
            "an unsatisfied Host-routed entry must merge into the prefix, not the real host"
        );
        Ok(())
    }
}
