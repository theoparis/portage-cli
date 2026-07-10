//! Emerge resolve-and-merge orchestration.

use std::str::FromStr;

use anyhow::bail;
use camino::Utf8Path;

use crate::cli;
use crate::error::{self, Result};
use crate::merge::confirm_merge;
use crate::merge::run_merge_plan;
use crate::vdb::open_cli_vdb;
use crate::{ebuild, preflight, query, search};

pub(crate) fn parse_atoms(raw: &[String]) -> Vec<portage_atom::Dep> {
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
    let mut out = Vec::with_capacity(raw.len());
    #[allow(unused_assignments)]
    // stack_holder's initial None may not be read if no sets are expanded
    let mut stack_holder: Option<portage_repo::ProfileStack> = None;
    let mut resolver: Option<portage_repo::SetResolver<'_>> = None;

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
                    // get_or_insert (not `stack_holder = Some(st); ...unwrap()`) hands
                    // back the `&ProfileStack` directly, so there's nothing to unwrap.
                    let stack = stack_holder.get_or_insert(st);
                    resolver = Some(portage_repo::SetResolver::new(stack, eroot));
                }
                Err(e) => {
                    eprintln!("warning: cannot expand @{name}: {e}");
                    // Cannot expand any sets if resolver creation failed; push raw string
                    out.push(s.clone());
                    continue;
                }
            }
        }

        // If we have a resolver, use it; otherwise skip (resolver creation failed earlier)
        if let Some(res) = resolver.as_ref() {
            match res.resolve(name) {
                Ok(atoms) => out.extend(atoms.iter().map(|d| d.to_string())),
                Err(e) => eprintln!("warning: skipping @{name}: {e}"),
            }
        } else {
            // Resolver creation failed for earlier set; push raw string
            out.push(s.clone());
        }
    }
    out
}
pub(crate) struct EmergeOpts<'a> {
    /// USE tokens (emerge syntax: `headers-only`, `-cxx`) forced on top of the
    /// configured USE, for both the resolve and the build (applied via the `USE`
    /// env, as `USE=… emerge` does).
    pub use_override: &'a [String],
    /// `--nodeps`: merge only the named atoms, no dependency expansion.
    pub nodeps: bool,
    /// Override depgraph flags (deep, newuse) for this call. When None, uses
    /// the values from cli.depgraph_flags.
    pub depgraph_flags: Option<crate::cli::DepgraphFlags>,
    /// Override merge-behavior flags (jobs, keep_going, buildpkg, …) for this
    /// call. When None, uses the values from `cli.merge_flags` — same
    /// override/fallback shape as `depgraph_flags` above, needed for the same
    /// reason: the staged driver (`crossdev::run_staged`) must see whichever
    /// of the subcommand's own flattened `MergeFlags` or the top-level one
    /// the user actually set (`em -j 80 stages --stage1` vs `em stages
    /// --stage1 -j 80`), not just the top-level `Cli`'s copy.
    pub merge_flags: Option<crate::cli::MergeFlags>,
    /// Install into the plain `--local`/`--prefix`/`--root` EROOT, ignoring any
    /// `--target` sysroot substitution. Needed for `cross-<CTARGET>/gcc` steps
    /// woven into a `--target`-active `stages --stage1` run: that package's
    /// eclass always installs under the outer EROOT (`crossdev/mod.rs`'s module
    /// doc), never the target sysroot subdirectory `roots()` would otherwise
    /// substitute in.
    pub bypass_cross_root: bool,
}

/// Resolve and (unless `--pretend`) merge `raw_atoms` with the global config in
/// `cli`, plus the per-call [`EmergeOpts`]. Factored out of [`run_emerge`] so the
/// crossdev staged-build driver can run each toolchain step through the very
/// same path.
pub(crate) async fn emerge_atoms(
    cli: &cli::Cli,
    raw_atoms: &[String],
    opts: EmergeOpts<'_>,
) -> Result<()> {
    // Apply the per-step USE override to the process env for the duration of the
    // step (restored after), so the resolve's USE-conditional expansion and the
    // build phases both see it. Mirrors crossdev's `USE="…" doemerge`.
    let saved_use = std::env::var("USE").ok();
    if !opts.use_override.is_empty() {
        let base = saved_use.clone().unwrap_or_default();
        let merged = format!("{base} {}", opts.use_override.join(" "));
        // SAFETY: the driver runs steps sequentially; no other task reads/writes
        // USE between this set and the restore below.
        unsafe { std::env::set_var("USE", merged.trim()) };
    }
    let result = emerge_atoms_inner(
        cli,
        raw_atoms,
        opts.nodeps,
        opts.depgraph_flags,
        opts.merge_flags,
        opts.bypass_cross_root,
    )
    .await;
    if !opts.use_override.is_empty() {
        // SAFETY: see above.
        unsafe {
            match &saved_use {
                Some(v) => std::env::set_var("USE", v),
                None => std::env::remove_var("USE"),
            }
        }
    }
    result
}

async fn emerge_atoms_inner(
    cli: &cli::Cli,
    raw_atoms: &[String],
    nodeps: bool,
    depgraph_flags_override: Option<crate::cli::DepgraphFlags>,
    merge_flags_override: Option<crate::cli::MergeFlags>,
    bypass_cross_root: bool,
) -> Result<()> {
    let merge_flags = merge_flags_override.as_ref().unwrap_or(&cli.merge_flags);
    let resolved = cli.repo_path();
    let repo_path = camino::Utf8Path::new(&resolved);
    if !repo_path.is_dir() {
        bail!("repo not found at {resolved}");
    }
    let repo = portage_repo::Repository::open(repo_path.as_std_path())?;
    let vdb = open_cli_vdb(cli).ok();
    let mode = if merge_flags.update {
        query::ResolveMode::PreferInstalled
    } else {
        query::ResolveMode::Error
    };
    // Root model (docs/root-model.md): config from roots.config (host for a
    // --prefix overlay), installed view = VDB(base) ∪ VDB(target), and the
    // plan installs into target. `bypass_cross_root` (woven-in `cross-*`
    // toolchain steps only) uses the plain outer EROOT instead — that
    // category always installs there, never into `--target`'s sysroot
    // substitution (see `crossdev/mod.rs`'s module doc).
    //
    // `outer_roots()`, not `base_roots()`: found live 2026-07-09 (independent
    // review) — `base_roots()`'s `merge_root()` is deliberately the *BROOT*
    // view (host `/` under `--prefix`, `base.target: None`), not the outer
    // EROOT this comment already says bypass steps need. Under `--root` the
    // two happen to coincide (no eprefix, `outer_roots()` returns
    // `base_roots()` unchanged), which is why this went unnoticed: every
    // `bypass_cross_root` case tested before today was `--root`. Under
    // `--prefix P`, `base_roots()` merged every crossdev toolchain step onto
    // the real host `/` instead of `P` — silently "worked" for binutils
    // (whose real-arch binaries just landed on host `/usr/bin`, harmless to
    // notice) but broke `linux-headers`/`glibc[headers-only]`, whose
    // build-against-sysroot path never saw the merged headers.
    let roots = if bypass_cross_root {
        cli.outer_roots()
    } else {
        cli.roots()
    };
    // `Cli::broot()` (not `roots`): stays overlay-aware under `--target`
    // substitution, so a `MergeRoot::Host` entry's `-p` display matches its
    // real merge destination even when `roots` has had its own overlay-ness
    // cleared by the sysroot substitution. See `DepgraphOpts::host_merge_root`.
    let host_roots = cli.broot();
    // `--target <tuple>` targets `<EROOT>/usr/<tuple>`; fail early with a setup
    // hint if that sysroot has not been laid down by `em crossdev --init-target`
    // (otherwise the profile/make.conf read fails with an opaque ENOENT). Skipped
    // for `bypass_cross_root`: those steps target the outer EROOT on purpose, not
    // the sysroot this check is guarding.
    if let Some(tuple) = cli.target.as_deref().filter(|_| !bypass_cross_root) {
        let cfg = roots.config().unwrap_or_else(|| camino::Utf8Path::new("/"));
        if !cfg.join("etc/portage/make.conf").exists() {
            bail!(
                "cross target '{tuple}' is not set up at {cfg}\n  \
                 run: em crossdev -t {tuple} --init-target"
            );
        }
    }
    // Expand @set references (e.g. @system, @world) to concrete atoms before
    // resolution. Sets are read from the config root's profile (@system) and
    // the merge target (@world/@selected, user sets).
    let expanded = expand_sets(raw_atoms, roots.config(), roots.merge_root());
    let parsed = query::resolve_atoms(&expanded, &repo, vdb.as_ref(), mode);
    let atoms: Vec<String> = parsed.iter().map(|d| d.to_string()).collect();
    if atoms.is_empty() {
        bail!("em: no valid atoms");
    }
    let format = if merge_flags.json {
        cli::DepgraphFormat::Json
    } else if merge_flags.tree {
        cli::DepgraphFormat::Tree
    } else {
        cli::DepgraphFormat::Pretty
    };
    let depgraph_flags = depgraph_flags_override
        .as_ref()
        .map(|f| (f.deep, f.newuse))
        .unwrap_or((cli.depgraph_flags.deep, cli.depgraph_flags.newuse));
    let outcome = query::depgraph::depgraph(query::depgraph::DepgraphOpts {
        repo_path,
        atoms: &atoms,
        arch: &cli.arch,
        format,
        verbose: cli.verbose,
        empty: merge_flags.emptytree,
        autounmask_write: merge_flags.autounmask_write,
        autosolve_use: merge_flags.autosolve_use,
        multi_repo: cli.repo.is_none(),
        roots: &roots,
        host_merge_root: host_roots.merge_root(),
        onlydeps: merge_flags.onlydeps,
        with_bdeps: merge_flags.with_bdeps,
        root_deps_rdeps: merge_flags.root_deps,
        deep: depgraph_flags.0,
        nodeps,
    })
    .await?;

    // A non-zero resolver exit means USE/mask changes are needed (the change
    // block was already printed). Surface it as a typed error so the normal
    // Result flow yields exit 1 — `main` prints it quietly, and the staged
    // driver stops at the step that needs the change, with step context.
    // Checked before the `--pretend` return (not just for a real run) so
    // `-p`/`-a` show the same signal a real run would hit.
    if outcome.exit_code != 0 {
        return Err(error::ConfigChangesNeeded.into());
    }

    if outcome.plan.is_empty() {
        return Ok(());
    }

    // Pre-flight: fail fast with a clear message if any plan entry's build
    // dependencies won't be present when it builds, rather than mid-build.
    // Run before the `--pretend` return (not just for a real run), so `-p`/
    // `-a` surface whether the plan is preflight-clean, the same way the
    // merge plan itself is already shown under `-p` — a preview could
    // otherwise never reveal a plan that would fail preflight during a real
    // run. Skipped under `--nodeps` regardless of `--pretend`: that flag
    // already means "merge only the named atoms, no dependency expansion or
    // verification" (matching emerge's own `--nodeps`) — the guard-rail
    // would otherwise still block on real, unconditional BDEPEND that
    // --nodeps deliberately opted out of checking (e.g. a genuine bootstrap
    // cycle like gawk -> bison -> gettext -> libxml2 -> meson -> python ->
    // gawk, which has no valid dependency order and must be seeded out of
    // order somewhere).
    if !nodeps {
        preflight::check(&outcome.plan, &roots, &outcome.provided)?;
    }

    if cli.pretend {
        return Ok(());
    }

    // --prefix additionally relocates distfiles and the build trees under the
    // target (a self-contained tree); --root leaves them at the host defaults.
    let relocate = roots.relocate().then(|| roots.merge_root());
    let distdir = relocate.map(|p| p.join("var/cache/distfiles"));
    let work_base = ebuild::default_work_base(relocate);

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
        merge_flags.keep_going,
        merge_flags.emptytree,
        merge_flags.jobs.map(|j| j as usize).unwrap_or(1).max(1),
        merge_flags.buildpkg,
        merge_flags.usepkg,
        merge_flags.getbinpkg,
        merge_flags.getbinpkgonly,
        cli,
    )
    .await
}

/// Run the default emerge path for a parsed CLI invocation.
pub(crate) async fn run_emerge(cli: &cli::Cli) -> Result<()> {
    // emerge -s / -S: the arguments are search patterns, not atoms.
    if cli.search || cli.searchdesc {
        return search::run_emerge_style(&cli.search_repos(), &cli.atoms, cli.searchdesc).await;
    }
    emerge_atoms(
        cli,
        &cli.atoms,
        EmergeOpts {
            use_override: &[],
            nodeps: cli.nodeps,
            depgraph_flags: None,
            merge_flags: None,
            bypass_cross_root: false,
        },
    )
    .await
}
