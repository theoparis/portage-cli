//! `em crossdev` — set up a cross-compilation target, a `crossdev` workalike.
//!
//! Implements the **no-build setup** (`--init-target` / `--show-target-cfg`):
//! overlay creation (the `cross-*` symlink category + `metadata`/`profiles` + a
//! `repos.conf` entry), the cross sysroot `make.conf`, and the **direct**
//! `make.profile` symlink (`eselect profile` refuses a foreign arch). `--setup`
//! additionally derives the ordered [`stages::toolchain_plan`] bootstrap
//! (binutils → headers → gcc-stage1 → libc → gcc-stage2) and runs each step
//! through the shared merge path.
//!
//! The staged-bootstrap driver ([`run_staged`]) and the [`stages::BootstrapKind`]
//! plan are shared with the **native toolchain** ([`toolchain`], `em toolchain
//! --setup`): a self-hosting toolchain into `--root` (`CHOST == CBUILD`) is the
//! same `glibc ↔ gcc` cycle as a cross toolchain, broken the same staged way —
//! see `todo/em-root-characterization.md`.
//!
//! The install location follows em's root model: the sysroot is
//! `<EROOT>/usr/<CTARGET>`, so `em crossdev <t>` targets `/usr/<CTARGET>` (like
//! crossdev), `em --local crossdev <t>` targets `~/.gentoo/usr/<CTARGET>`, and
//! `em --prefix DIR`/`--root DIR` retarget under `DIR`.
//!
//! ## `cross-<CTARGET>/gcc` vs `sys-devel/gcc` — two different packages
//!
//! Easy to conflate, and doing so caused real confusion chasing a stage1
//! failure (`todo/stage-build-shakeout.md` finding #19): they are **not** the
//! same compiler at any point.
//!
//! - **`cross-<CTARGET>/gcc`** (this module's overlay category, built by
//!   [`stages::toolchain_plan`]) is the **host-side cross-compiler**: it runs
//!   on `CBUILD`, emits code for `CTARGET`, and is what every ordinary
//!   package's `PATH` resolves `<CTARGET>-gcc`/`riscv64-unknown-linux-gnu-gcc`
//!   to via `gcc-config` (see `env_d.rs`). It's built once during
//!   `--setup`/`--init-target` and only changes if you explicitly rebuild or
//!   upgrade it — nothing else in `em` re-solves or upgrades it implicitly.
//! - **`sys-devel/gcc`** is the ordinary, real-category ebuild for "the
//!   compiler built with `CHOST == CTARGET`" — i.e. a compiler that will
//!   *itself run on* whatever `CHOST` currently is, no matter which host that
//!   happens to be. Installed via `em stages --stage1`/plain `em` merges, its
//!   version is resolved completely independently of `cross-<CTARGET>/gcc`.
//!
//! Because these are separate, independently-resolved atoms, they can drift:
//! `em stages --stage1 --target <t>` installing a newer `sys-devel/gcc` into
//! the target sysroot does **not** upgrade the `cross-<t>/gcc` cross-compiler
//! actually used to *build* it — and GCC cannot reliably self-bootstrap a
//! newer major version using an older one as `CC_FOR_TARGET` (a real GCC
//! limitation, not an em bug). Keeping the two in sync is a `--update`/rebuild
//! concern — see `todo/stage-build-shakeout.md` finding #19 for the pending
//! `crossdev --update` support and version-mismatch warning.

mod multilib;
pub mod stages;
mod target;

use std::io::Write;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::{MakeConf, ProfileStack, ReposConf, Repository};

use crate::cli::{Cli, CrossdevArgs, DepgraphFlags, MergeFlags};
use crate::style::{C_LABEL, C_PKG};
use crate::util::write_if_absent;
use target::CrossTarget;

/// Merge a subcommand's own flattened depgraph flags with the top-level one,
/// args taking precedence — so `--deep`/`--newuse` work whether given before
/// or after the subcommand name.
fn merge_depgraph_flags(globals: &Cli, args: &DepgraphFlags) -> DepgraphFlags {
    DepgraphFlags {
        deep: args.deep || globals.depgraph_flags.deep,
        newuse: args.newuse || globals.depgraph_flags.newuse,
    }
}

/// Merge merge-behavior flags (`-j`, `--keep-going`, `--buildpkg`, …) from a
/// subcommand's own flattened [`MergeFlags`] with the top-level one, args
/// taking precedence — the same "either position works" merge
/// [`merge_depgraph_flags`] already does for `--deep`/`--newuse`, needed here
/// for the same reason (`em -j 80 stages --stage1` vs `em stages --stage1 -j
/// 80`, see `todo/stage-build-shakeout.md`).
fn merge_merge_flags(globals: &Cli, args: &MergeFlags) -> MergeFlags {
    let g = &globals.merge_flags;
    MergeFlags {
        update: args.update || g.update,
        autounmask_write: args.autounmask_write || g.autounmask_write,
        oneshot: args.oneshot || g.oneshot,
        fetchonly: args.fetchonly || g.fetchonly,
        buildpkg: args.buildpkg || g.buildpkg,
        usepkg: args.usepkg || g.usepkg,
        usepkgonly: args.usepkgonly || g.usepkgonly,
        getbinpkg: args.getbinpkg || g.getbinpkg,
        getbinpkgonly: args.getbinpkgonly || g.getbinpkgonly,
        emptytree: args.emptytree || g.emptytree,
        tree: args.tree || g.tree,
        json: args.json || g.json,
        onlydeps: args.onlydeps || g.onlydeps,
        noreplace: args.noreplace || g.noreplace,
        jobs: args.jobs.or(g.jobs),
        load_average: args.load_average.or(g.load_average),
        keep_going: args.keep_going || g.keep_going,
        autounmask: args.autounmask || g.autounmask,
        autosolve_use: args.autosolve_use || g.autosolve_use,
        complete_graph: args.complete_graph || g.complete_graph,
        with_bdeps: args.with_bdeps || g.with_bdeps,
        exclude: if args.exclude.is_empty() {
            g.exclude.clone()
        } else {
            args.exclude.clone()
        },
        root_deps: args.root_deps || g.root_deps,
    }
}

/// The overlay name crossdev uses — one overlay holds every `cross-*` category.
const OVERLAY_NAME: &str = "crossdev";

pub async fn run(args: &CrossdevArgs, globals: &Cli) -> Result<()> {
    // `--target` is global (`Cli`, not `CrossdevArgs`): one flag, read the
    // same way whether it's setting a target up (here) or using an
    // already-set-up one elsewhere (`stages --stage1`, plain atom builds) —
    // not two separate flags that can disagree.
    let tuple = globals
        .target
        .clone()
        .ok_or_else(|| anyhow::anyhow!("em crossdev needs a target tuple: pass --target/-T"))?;
    let target = CrossTarget::parse(&tuple, args.llvm)?;

    if args.show_target_cfg {
        return show_target_cfg(&target, globals);
    }
    if args.init_target {
        return init_target(&target, globals);
    }
    if args.setup {
        return setup(&target, globals, args).await;
    }
    bail!(
        "em crossdev does setup only for now — pass --init-target to lay down the \
         overlay + sysroot config, --setup to bootstrap the cross toolchain, or \
         --show-target-cfg to preview the derived config"
    );
}

/// `em crossdev <tuple> --setup`: bootstrap the cross toolchain into the prefix
/// (`/usr/<chost>`). The full intertwined sequence (binutils → headers →
/// gcc-stage1 → libc → gcc-stage2) — the compiler is not usable until the libc
/// step lands, so toolchain and stage1 libc are one bootstrap.
///
/// Lays down the FS config (idempotent), then runs each step of the ordered
/// [`StagePlan`](stages::StagePlan) through the shared merge path
/// ([`crate::emerge_atoms`]) — per-step `USE` override + `--nodeps`. With `-p`
/// each step prints its plan instead of building.
async fn setup(target: &CrossTarget, globals: &Cli, args: &CrossdevArgs) -> Result<()> {
    // `-p` only previews the staged builds — don't write the overlay/sysroot.
    if !globals.pretend {
        init_target(target, globals)?;
    }
    // A self-contained `--root DIR` EPREFIX has no host-shared merged-usr
    // skeleton or libs, so the plan needs the same from-scratch treatment as
    // native. `outer_roots()`, not `roots()`: this must stay anchored to the
    // outer EROOT even if `--target` happens to also be set on this
    // invocation.
    let self_contained = globals.outer_roots().is_self_contained_root();
    let plan = stages::toolchain_plan(
        &stages::BootstrapKind::Cross(target.clone()),
        self_contained,
    );
    let mut out = anstream::stdout();
    let verb = if globals.pretend { "Plan" } else { "Bootstrap" };
    writeln!(
        out,
        "\n{C_LABEL}{verb} cross toolchain{C_LABEL:#} ({}) — {} steps:",
        target.tuple,
        plan.steps.len()
    )
    .ok();

    let post_step = {
        let target = target.clone();
        move |step: &stages::StageStep| post_step_cross(&target, globals, step)
    };
    // `--root-deps=rdeps` unconditionally: the whole point of this bootstrap is
    // building a toolchain (+ glibc) into a target that starts empty, where
    // plain DEPEND (`virtual/os-headers`, `acct-group/root`, …) genuinely can't
    // be satisfied yet. Matches crossdev's own `<CTARGET>-emerge` wrapper,
    // which always implies this flag — not user-togglable here.
    let mut merge_flags = merge_merge_flags(globals, &args.merge_flags);
    merge_flags.root_deps = true;
    // `bypass_cross_root: true` — this is `crossdev --setup <T>`, which since
    // the `--cross`/`-t` -> `--target` unification (`bcde18a`) always runs
    // with the global `--target` flag active (that's now the only way to
    // name the tuple). Before that unification the tuple came via crossdev's
    // own separate `-t` flag, which never touched `globals.target`, so
    // `cli.roots()` here was already the unsubstituted outer EROOT and
    // `false` was harmless; afterwards it silently started resolving every
    // toolchain-bootstrap step (`cross-<T>/binutils` and its own deps)
    // against the *sysroot* instead — reading the sysroot's target-arch
    // make.conf (`CHOST`/`CFLAGS=-march=...`) to build a package that must
    // compile as a *host*-arch tool. Found live 2026-07-09 actually running
    // `crossdev --setup` post-unification for the first time (only
    // `--init-target`, which never reaches `run_staged`, had been
    // live-tested since).
    run_staged(
        &plan,
        globals,
        merge_depgraph_flags(globals, &args.depgraph_flags),
        merge_flags,
        true,
        post_step,
    )
    .await?;

    if !globals.pretend {
        writeln!(
            out,
            "\n>>> cross toolchain {} ready in {}/usr/{}",
            target.tuple,
            globals.outer_roots().merge_root(),
            target.tuple,
        )
        .ok();
    }
    Ok(())
}

/// Cross post-step hook: activate the freshly-built toolchain
/// (`<CTARGET>-*` wrappers via `binutils-config`/`gcc-config`), and after the
/// full libc lands (not the headers-only bootstrap step) bridge the ABI osdir
/// symlinks so the next gcc step links target code against it.
fn post_step_cross(target: &CrossTarget, globals: &Cli, step: &stages::StageStep) -> Result<()> {
    activate_toolchain(target, globals, step)?;
    if step.label == "libc" {
        link_abi_osdirs(target, globals)?;
    }
    Ok(())
}

/// Run each step of a staged [`stages::StagePlan`] through the shared merge path
/// ([`crate::emerge_atoms`]), printing per-step progress. `post_step` fires
/// after each *built* step (skipped under `-p`) for flavour-specific activation
/// — cross activates `<CTARGET>-*` wrappers + ABI osdirs; native is a no-op.
/// This is the shared driver both `--setup` (cross) and `stage1` (native) run.
///
/// `bypass_cross_root` forces every step's merge into the plain outer EROOT
/// even when `globals.target` is set — for `cross-*` toolchain plans woven
/// into a `--target`-active `stage1` run (see `maybe_weave_in_gcc_update`),
/// which must never install under `--target`'s sysroot substitution.
async fn run_staged(
    plan: &stages::StagePlan,
    globals: &Cli,
    depgraph_flags: crate::cli::DepgraphFlags,
    merge_flags: MergeFlags,
    bypass_cross_root: bool,
    post_step: impl Fn(&stages::StageStep) -> Result<()>,
) -> Result<()> {
    let mut out = anstream::stdout();
    for (i, step) in plan.steps.iter().enumerate() {
        // Flush the step header before building so progress shows immediately
        // (and survives the `process::exit` on a step that needs config changes,
        // which does not flush buffered stdout). The header names the step, so a
        // failure needs no extra context.
        writeln!(
            out,
            "\n{C_LABEL}[{n}/{total}] {label}{C_LABEL:#}{flags}",
            n = i + 1,
            total = plan.steps.len(),
            label = step.label,
            flags = step_flags(step),
        )
        .ok();
        out.flush().ok();
        crate::emerge_atoms(
            globals,
            &step.atoms,
            crate::EmergeOpts {
                use_override: &step.use_override,
                nodeps: step.nodeps,
                depgraph_flags: Some(depgraph_flags.clone()),
                merge_flags: Some(merge_flags.clone()),
                bypass_cross_root,
            },
        )
        .await?;

        if !globals.pretend {
            post_step(step)?;
        }
    }
    Ok(())
}

/// Whether `atom`'s package name is `pkg` — handles both a bare atom
/// (`cross-<T>/gcc`) and a version-pinned one (`=cross-<T>/gcc-16.1.1...`,
/// as [`stages::gcc_refresh_plan`] uses to force an exact upgrade rather than
/// a same-version reinstall). A bare `ends_with("/gcc")` check misses the
/// pinned form entirely — caught live: it silently skipped activating the
/// freshly-built compiler, leaving the *old* slot active for the very build
/// this refresh existed to fix.
fn atom_is_package(atom: &str, pkg: &str) -> bool {
    match atom.rsplit_once('/') {
        Some((_, rest)) => {
            rest == pkg
                || rest
                    .strip_prefix(pkg)
                    .and_then(|v| v.strip_prefix('-'))
                    .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
        }
        None => false,
    }
}

/// Run the prefix-side `binutils-config`/`gcc-config` after the step that built
/// the tool, creating the `<EROOT>/usr/bin/<CTARGET>-*` wrappers. Keyed off the
/// step's package so it fires once per toolchain component.
///
/// Always activates against `globals.outer_roots()`, never `globals.roots()`:
/// `cross-<CTARGET>/*` toolchain packages always install into the plain outer
/// EROOT (see this module's doc comment), regardless of whether the *caller*
/// (`setup()` vs `stage1()`'s woven-in refresh) has `--target` set on
/// `globals` for its own, unrelated purposes. For `setup()` the two are the
/// same root anyway (it never sets `--target`), so this is a no-op change
/// there.
///
/// `outer_roots()`, not `base_roots()`: found live 2026-07-09 alongside the
/// matching `emerge.rs` fix — `base_roots()`'s `merge_root()` is the BROOT
/// view (host `/` under `--prefix`), not the outer EROOT the toolchain
/// actually merges into after that fix. Activating against the wrong root
/// would look for the just-built binutils/gcc in the wrong place under
/// `--prefix` specifically (host `/`, not the prefix).
fn activate_toolchain(target: &CrossTarget, globals: &Cli, step: &stages::StageStep) -> Result<()> {
    let Some(atom) = step.atoms.first() else {
        return Ok(());
    };
    let tuple = &target.tuple;
    let roots = globals
        .outer_roots()
        .with_own_config_root_if_self_contained();
    let activated = if atom_is_package(atom, "binutils") {
        crate::select::activate_binutils(&roots, tuple)?
    } else if atom_is_package(atom, "gcc") {
        crate::select::activate_compiler(&roots, tuple)?
    } else {
        return Ok(());
    };
    if activated {
        println!("    activated {} for {tuple}", step.label);
    }
    Ok(())
}

/// Render a step's USE override / `--nodeps` as a compact suffix for the plan.
fn step_flags(step: &stages::StageStep) -> String {
    let mut parts = Vec::new();
    if step.nodeps {
        parts.push("--nodeps".to_string());
    }
    if !step.use_override.is_empty() {
        parts.push(format!("USE=\"{}\"", step.use_override.join(" ")));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  [{}]", parts.join(" "))
    }
}

/// `em toolchain --setup`: bootstrap a self-hosting native toolchain into
/// `--root` (`CHOST == CBUILD`, `SYSROOT == ROOT`). The native twin of the
/// crossdev `--setup`, sharing its staged driver but with the *native* plan
/// (baselayout → binutils → os-headers → full glibc → full gcc): the seed
/// compiler at `BROOT=/` builds glibc directly, so there is no two-stage gcc
/// (that is cross-only — see [`stages`]). Plain `::gentoo` atoms, none of the
/// cross overlay/wrapper/sysroot-make.conf ceremony — the host profile and
/// make.conf configure it (`--config-root /` by default).
///
/// This is the *toolchain* primitive only — the compiler the stages build
/// against. The actual stage production (stage1 `packages.build`, stage3
/// `--emptytree @system`) lives in `em stages` (see
/// `todo/em-stages-and-binhosts.md`). Requires `--root <dir>` (a toolchain into
/// `/` is meaningless). With `-p` each step prints its plan instead of building.
pub(crate) async fn toolchain(args: &crate::cli::ToolchainArgs, globals: &Cli) -> Result<()> {
    if !args.setup {
        bail!(
            "em toolchain does setup only for now — pass --setup to bootstrap the \
             native toolchain into --root"
        );
    }
    // outer_roots(), not roots(): a native toolchain bootstrap must anchor to
    // the outer EROOT even if a global --target happens to also be set.
    let merge_root = globals.outer_roots().merge_root().to_owned();
    if merge_root.as_str() == "/" {
        bail!(
            "em toolchain --setup needs --root <dir>: a native toolchain into / is \
             meaningless (use the host toolchain directly, or pass --root <empty>)"
        );
    }
    if !globals.pretend {
        ensure_self_contained_prefix(globals)?;
    }
    let plan = stages::toolchain_plan(&stages::BootstrapKind::Native, true);
    let mut out = anstream::stdout();
    let verb = if globals.pretend { "Plan" } else { "Bootstrap" };
    writeln!(
        out,
        "\n{C_LABEL}{verb} native toolchain{C_LABEL:#} into {merge_root} — {} steps:",
        plan.steps.len()
    )
    .ok();
    run_staged(
        &plan,
        globals,
        merge_depgraph_flags(globals, &args.depgraph_flags),
        merge_merge_flags(globals, &args.merge_flags),
        false,
        |_| Ok(()),
    )
    .await?;
    if !globals.pretend {
        writeln!(out, "\n>>> native toolchain ready in {merge_root}").ok();
    }
    Ok(())
}

/// `em stages --stage1`: emerge the profile's `packages.build` bootstrap set
/// into `--root` — baselayout (USE=build, `--nodeps`) then the minimal stage1
/// package list (USE="-* build"), mirroring catalyst's `stage1/chroot.sh`.
/// Requires the ROOT's own toolchain already built (`em toolchain --setup`);
/// stage1 assumes a working `<chost>-gcc` is already in the root, it does not
/// build one (that's [`toolchain`]). With `-p` it prints the plan instead of
/// building.
pub(crate) async fn stage1(args: &crate::cli::StagesArgs, globals: &Cli) -> Result<()> {
    if !args.stage1 {
        bail!(
            "em stages does stage1 only for now — pass --stage1 to emerge the \
             profile's packages.build bootstrap set into --root"
        );
    }
    let merge_root = globals.roots().merge_root().to_owned();
    if merge_root.as_str() == "/" {
        bail!("em stages --stage1 needs --root <dir>: a stage1 into / is meaningless");
    }
    let stack = profile_stack(globals)?;
    let plan = stages::stage1_plan(&stack)?;
    let refresh = maybe_weave_in_gcc_update(&stack, globals).await;
    let mut out = anstream::stdout();
    let verb = if globals.pretend { "Plan" } else { "Bootstrap" };

    // The `cross-<CTARGET>/gcc` refresh (if needed) is a separate run: it
    // always installs into the outer EROOT (`bypass_cross_root: true`),
    // never `--target`'s sysroot substitution the stage1 packages below use.
    if let Some((target, refresh_plan)) = &refresh {
        writeln!(
            out,
            "\n{C_LABEL}{verb} cross-compiler refresh{C_LABEL:#} ({}) — {} steps:",
            target.tuple,
            refresh_plan.steps.len()
        )
        .ok();
        let post_step = {
            let target = target.clone();
            move |step: &stages::StageStep| post_step_cross(&target, globals, step)
        };
        run_staged(
            refresh_plan,
            globals,
            merge_depgraph_flags(globals, &args.depgraph_flags),
            merge_merge_flags(globals, &args.merge_flags),
            true,
            post_step,
        )
        .await?;
    }

    writeln!(
        out,
        "\n{C_LABEL}{verb} native stage1{C_LABEL:#} into {merge_root} — {} steps:",
        plan.steps.len()
    )
    .ok();
    run_staged(
        &plan,
        globals,
        merge_depgraph_flags(globals, &args.depgraph_flags),
        merge_merge_flags(globals, &args.merge_flags),
        false,
        |_| Ok(()),
    )
    .await?;
    if !globals.pretend {
        writeln!(out, "\n>>> stage1 ready in {merge_root}").ok();
    }
    Ok(())
}

/// If this is a cross build and the stage1 set includes `sys-devel/gcc`,
/// check whether `gcc-config`'s currently *active* `cross-<CTARGET>/gcc` is
/// new enough to build it, and if not, return a
/// [`stages::gcc_refresh_plan`] to run (into the outer EROOT, via
/// `bypass_cross_root` — see [`run_staged`]) before the stage1 plan itself.
///
/// `sys-devel/gcc` (`CHOST == CTARGET`) builds single-pass, not as a
/// self-hosting bootstrap (`toolchain.eclass`'s `is_crosscompile()` is false
/// for it) — the active cross-compiler is its *only* build tool. GCC's own
/// target libraries (e.g. `libatomic`) can pass driver flags only a
/// matching-or-newer major version understands, so an older active
/// cross-compiler silently breaks deep inside a target library's own
/// `configure` — see `todo/stage-build-shakeout.md`.
///
/// Best-effort: any failure determining compatibility (no active compiler
/// yet is the *expected* "needs building" case and always weaves in; an
/// unparseable slot, an LLVM cross target with no `cross-<CTARGET>/gcc`
/// package at all, or a resolve failure are all treated as "can't tell,
/// leave the plan alone" rather than blocking the stage1 run).
async fn maybe_weave_in_gcc_update(
    stack: &ProfileStack,
    globals: &Cli,
) -> Option<(CrossTarget, stages::StagePlan)> {
    let tuple = globals.target.clone()?;
    let stage1_atoms = stack.stage1_packages().ok()?;
    if !stage1_atoms.iter().any(|d| d.cpn.package.as_str() == "gcc") {
        return None;
    }
    let needed_version = resolve_gcc_version(globals).await?;
    let needed_slot = needed_version.split(['.', '_']).next()?;
    let target = CrossTarget::parse(&tuple, false).ok()?;
    let active_slot = crate::select::current_compiler_slot(
        &globals
            .outer_roots()
            .with_own_config_root_if_self_contained(),
        &target.tuple,
    );
    if gcc_needs_refresh(active_slot.as_deref(), needed_slot) {
        let refresh_plan = stages::gcc_refresh_plan(&target, &needed_version);
        Some((target, refresh_plan))
    } else {
        None
    }
}

/// Whether the active cross-compiler slot is too old to build a
/// `needed_slot` `sys-devel/gcc`: nothing activated yet (`None`) or a
/// strictly older slot. A newer-or-equal active slot is assumed fine (GCC is
/// generally backward compatible as a *build tool*, and this is a numeric
/// gate, not exact-match, to avoid gratuitous rebuilds). An unparseable
/// slot (either side) is treated as "can't tell" rather than "needs
/// refresh" — GCC's own SLOT is always a plain integer, so this should never
/// actually happen; if it does, silently doing nothing is safer than
/// forcing an unwanted rebuild.
fn gcc_needs_refresh(active_slot: Option<&str>, needed_slot: &str) -> bool {
    let Ok(needed_num) = needed_slot.parse::<u32>() else {
        return false;
    };
    match active_slot {
        None => true,
        Some(active) => active.parse::<u32>().is_ok_and(|n| n < needed_num),
    }
}

/// The exact version `sys-devel/gcc` would resolve to for this invocation's
/// config (`ACCEPT_KEYWORDS`/masks), e.g. `"16.1.1_p20260606"`. A lightweight
/// `--nodeps` resolve of the single atom, reusing the same `depgraph()`
/// machinery every merge already goes through. GCC's own `SLOT` is always
/// its major version (`gcc.eclass`: `SLOT="$(ver_cut 1)"`), so callers needing
/// just the slot take the version's first component.
async fn resolve_gcc_version(globals: &Cli) -> Option<String> {
    let repo_path_str = globals.repo_path();
    let roots = globals.roots();
    // See `DepgraphOpts::host_merge_root`: `Cli::broot()` stays overlay-aware
    // under `--target` substitution, unlike `roots`.
    let host_roots = globals.broot();
    let outcome = crate::query::depgraph::depgraph(crate::query::depgraph::DepgraphOpts {
        repo_path: Utf8Path::new(&repo_path_str),
        atoms: &["sys-devel/gcc".to_string()],
        arch: &globals.arch,
        format: crate::cli::DepgraphFormat::Pretty,
        verbose: 0,
        empty: false,
        autounmask_write: false,
        autosolve_use: false,
        multi_repo: globals.repo.is_none(),
        roots: &roots,
        host_merge_root: host_roots.merge_root(),
        onlydeps: false,
        with_bdeps: false,
        root_deps_rdeps: false,
        deep: false,
        nodeps: true,
    })
    .await
    .ok()?;
    let merge = outcome
        .plan
        .iter()
        .find(|m| m.cpv.cpn.category == "sys-devel" && m.cpv.cpn.package == "gcc")?;
    Some(merge.cpv.version.to_string())
}

/// Build the [`ProfileStack`] for the invocation's config-root (host `/`
/// unless `--config-root`/`--root` offsets it), resolving
/// `etc/portage/make.profile` the same way `@system`/`@world` expansion does.
fn profile_stack(globals: &Cli) -> Result<ProfileStack> {
    let roots = globals.roots();
    let config_root = roots.config().unwrap_or(Utf8Path::new("/"));
    let profile_link = config_root.join("etc/portage/make.profile");
    let canon = std::fs::canonicalize(profile_link.as_std_path())
        .with_context(|| format!("cannot resolve make.profile at {profile_link}"))?;
    ProfileStack::build(canon).context("failed to build profile stack")
}

/// `EROOT`/prefix the overlay, `repos.conf`, and `package.env` are written under
/// (`~/.gentoo` for `--local`), so an unprivileged setup is writable + readable.
///
/// `outer_roots()`, not `roots()`: this is the outer EROOT the overlay lives
/// in, which must stay stable even if a global `--target` happens to also be
/// set on the invocation (`roots()` would already be `--target`'s sysroot
/// substitution, doubly-nesting anything joined onto it below).
fn setup_root(globals: &Cli) -> Utf8PathBuf {
    globals.outer_roots().merge_root().to_owned()
}

/// The target sysroot `<EROOT>/usr/<CTARGET>` (EROOT = `/` by default, the prefix
/// for `--local`/`--prefix`, the root for `--root`).
fn sysroot(target: &CrossTarget, globals: &Cli) -> Utf8PathBuf {
    globals
        .outer_roots()
        .merge_root()
        .join("usr")
        .join(&target.tuple)
}

/// The configured main repo (`gentoo`) — the real ebuilds the overlay links to.
///
/// A self-contained `--root DIR` target (no `--local`/`--prefix` host-config
/// sharing) starts with no `repos.conf` of its own — that's exactly the
/// "stage1 from scratch" case, and `--init-target` is what's supposed to lay
/// one down. So this can't rely solely on the target's own config-root: it
/// falls back to the *host's* `repos.conf`, then to portage's own well-known
/// default location (mirroring `Cli::repo_path`'s fallback), so the very first
/// `--init-target` on a fresh root can still find the real ebuild tree to
/// symlink/reference.
fn main_repo(globals: &Cli) -> Result<Repository> {
    let target_conf = globals.outer_roots().repos_conf().ok();
    let host_conf = ReposConf::load_rooted(Utf8Path::new("/"), &[]).ok();
    let entry = target_conf
        .as_ref()
        .and_then(|c| c.main_repo().or_else(|| c.find("gentoo")))
        .or_else(|| {
            host_conf
                .as_ref()
                .and_then(|c| c.main_repo().or_else(|| c.find("gentoo")))
        });
    match entry {
        Some(e) => Repository::open(e.location.as_path().unwrap_or(std::path::Path::new(".")))
            .with_context(|| format!("opening main repo at {}", e.location.as_path().map(|p| p.display().to_string()).unwrap_or_else(|| "(virtual)".to_string()))),
        None => Repository::open("/var/db/repos/gentoo")
            .context("no main repo configured in repos.conf (target or host) and the default /var/db/repos/gentoo is not a repo either"),
    }
}

fn show_target_cfg(target: &CrossTarget, globals: &Cli) -> Result<()> {
    let mut out = anstream::stdout();
    let row = |out: &mut dyn Write, k: &str, v: &str| {
        writeln!(out, "  {C_LABEL}{k:<9}{C_LABEL:#} {v}").ok();
    };
    let model = if target.llvm { "LLVM/Clang" } else { "GCC" };
    row(&mut out, "Target", &target.tuple);
    row(&mut out, "Model", model);
    row(&mut out, "Category", &target.category());
    row(&mut out, "ARCH", &target.gentoo_arch());
    row(&mut out, "Profile", &target.profile_path());
    row(&mut out, "Sysroot", sysroot(target, globals).as_str());
    row(&mut out, "CFLAGS", target.cflags());
    writeln!(out, "  {C_LABEL}Packages{C_LABEL:#}").ok();
    let category = target.category();
    for (cat, pkg, _) in target.packages() {
        writeln!(out, "    {C_PKG}{category}/{pkg}{C_PKG:#} → {cat}/{pkg}").ok();
    }
    Ok(())
}

fn init_target(target: &CrossTarget, globals: &Cli) -> Result<()> {
    // For a retargeted prefix (`--local`/`--prefix`/`--root`) bootstrap it first:
    // `setup::bootstrap` writes the prefix `bashrc` that re-adds `<EROOT>/usr/bin`
    // to the build PATH (the shell sanitiser strips `$HOME` paths, so a `--local`
    // prefix's own bin is otherwise invisible). That is what makes the cross
    // toolchain wrappers we install reachable by the gcc-stage builds. Idempotent.
    let gentoo_path = ensure_self_contained_prefix(globals)?;
    let sysroot = sysroot(target, globals);
    let category = target.category();

    // Derive the cross packages on the fly: a `Location::Alias` repos.conf
    // entry declares `cross-<tuple>/<pkg>` as a virtual alias for its real
    // `::gentoo` package, materialised in-memory at load time. No on-disk
    // symlink overlay. See todo/cross-derive-on-the-fly.md.
    write_alias_repo_conf(globals, &gentoo_path, target, &category)?;
    write_cross_env(target, globals, &gentoo_path)?;
    write_sysroot_config(
        target,
        &sysroot,
        globals.outer_roots().merge_root(),
        &gentoo_path,
    )?;
    write_sysroot_repos_conf(&sysroot, &gentoo_path, target, &category)?;

    println!(">>> cross target {} ready", target.tuple);
    println!("    alias:     {category}  (derived from ::gentoo)");
    println!("    sysroot:  {sysroot}");
    // The toolchain itself is a HOST build (compiler lands on /), so it resolves
    // with host config — NOT the sysroot (that fights the cross make.conf ROOT).
    println!(
        "    toolchain: em -p {}/gcc          # host build of the cross compiler",
        target.category()
    );
    Ok(())
}

/// Write the virtual `Location::Alias` repos.conf entry that derives
/// `cross-<tuple>/<pkg>` packages from `::gentoo` at resolve time — the
/// in-memory replacement for the old on-disk symlink overlay. The entry maps
/// the destination cross category to the real `(category, package)` set from
/// [`CrossTarget::packages`], the single source of truth. See
/// `todo/cross-derive-on-the-fly.md`.
///
/// The real packages' existence under `gentoo` is verified up front (a missing
/// source package would later surface as a resolver `NoVersions` with no hint
/// at the cause); the alias declaration itself is always written so a partial
/// tree still resolves the packages that *are* present.
fn write_alias_repo_conf(
    globals: &Cli,
    gentoo: &Utf8Path,
    target: &CrossTarget,
    category: &str,
) -> Result<()> {
    // Validate every source package exists under ::gentoo, with a clear error
    // naming the cross package it's needed for, before declaring the alias.
    for (real_cat, pkg, _) in target.packages() {
        let dst = gentoo.join(real_cat).join(pkg);
        if !dst.is_dir() {
            bail!("{real_cat}/{pkg} not found at {dst} (needed for {category}/{pkg})");
        }
    }

    let conf_dir = setup_root(globals).join("etc/portage/repos.conf");
    std::fs::create_dir_all(&conf_dir).with_context(|| format!("creating {conf_dir}"))?;
    // Don't clobber an existing crossdev entry (crossdev/eselect may provide
    // one); only write if absent or its target/category drifted.
    let packages_line = alias_packages_line(target);
    let body = format!(
        "[{OVERLAY_NAME}]\nalias-source = gentoo\nalias-target = {category}\n\
         alias-packages = {packages_line}\n"
    );
    let file = conf_dir.join(format!("{OVERLAY_NAME}.conf"));
    if let Ok(existing) = std::fs::read_to_string(&file)
        && existing.contains(&format!("alias-target = {category}"))
        && existing.contains(&format!("alias-packages = {packages_line}"))
    {
        return Ok(());
    }
    write_if_absent(&file, &body)
}

/// Bootstrap the EPREFIX config that both `em toolchain --setup` (native) and
/// `em crossdev --setup` (cross) need before merging anything into it:
/// - the skeleton (`setup::bootstrap`, idempotent);
/// - a `gentoo` `repos.conf` entry, for a self-contained `--root DIR` target
///   only (`roots.is_self_contained_root()` — unlike `--local`/`--prefix`,
///   which merge this same directory onto the host's real repos.conf as an
///   extra source, so already resolve `gentoo` from there);
/// - a `make.profile` link, same self-contained-only condition — the EPREFIX
///   builds *host-arch* packages (the crossdev toolchain lands on
///   `ROOT=/`-equivalent, and a native toolchain always is host-arch), so it
///   links the *host's* resolved profile, unlike the cross target sysroot,
///   which links the target's own arch profile.
///
/// Without this a self-contained `--root` target has no way to resolve any
/// ebuild at all — the "stage1 from scratch" gap found 2026-07-03 doing a
/// real from-scratch native + cross toolchain bootstrap, see
/// [[stage-build-shakeout]]. Returns the resolved `::gentoo` repo path.
fn ensure_self_contained_prefix(globals: &Cli) -> Result<Utf8PathBuf> {
    let roots = globals.outer_roots();
    if roots.merge_root().as_str() != "/" {
        crate::setup::bootstrap(&roots)?;
    }
    let gentoo_path = main_repo(globals)?.path().to_owned();
    if roots.is_self_contained_root() {
        let conf_dir = setup_root(globals).join("etc/portage/repos.conf");
        std::fs::create_dir_all(&conf_dir).with_context(|| format!("creating {conf_dir}"))?;
        write_if_absent(
            &conf_dir.join("gentoo.conf"),
            &format!("[gentoo]\nlocation = {gentoo_path}\n"),
        )?;
        ensure_prefix_profile(globals)?;
    }
    Ok(gentoo_path)
}

/// The whitespace-separated real-cpn list for `alias-packages`, in stage
/// order (matching [`CrossTarget::packages`]). The parser re-parses each
/// token as a `Cpn`, so this is pure config-file serialisation — no identity
/// is carried as an opaque string downstream.
fn alias_packages_line(target: &CrossTarget) -> String {
    target
        .packages()
        .into_iter()
        .map(|(cat, pkg, _)| format!("{cat}/{pkg}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Link a `make.profile` for a self-contained `--root DIR` EPREFIX (same
/// "stage1 from scratch" gap as [`ensure_repos_conf`]'s `gentoo.conf`): unlike
/// `--local`/`--prefix`, which share the host's own `make.profile` via config
/// sharing, plain `--root` has none of its own. The EPREFIX builds *host-arch*
/// packages (the crossdev toolchain lands on `ROOT=/`-equivalent, just
/// offset), so — unlike the target sysroot, which links the target's own
/// arch profile — this links the *host's* resolved profile. A no-op for
/// `--local`/`--prefix` (`!roots.is_self_contained_root()`, config already
/// comes from the host).
fn ensure_prefix_profile(globals: &Cli) -> Result<()> {
    if !globals.outer_roots().is_self_contained_root() {
        return Ok(());
    }
    let link = setup_root(globals).join("etc/portage/make.profile");
    if link.exists() {
        return Ok(());
    }
    let host_profile = std::fs::canonicalize("/etc/portage/make.profile")
        .context("resolving the host's make.profile")?;
    let host_profile = Utf8PathBuf::from_path_buf(host_profile)
        .map_err(|p| anyhow::anyhow!("host make.profile path {p:?} is not valid UTF-8"))?;
    symlink_force(&host_profile, &link)
}

/// Write the cross sysroot `etc/portage/{make.conf,make.profile}`.
fn write_sysroot_config(
    target: &CrossTarget,
    sysroot: &Utf8Path,
    outer_root: &Utf8Path,
    gentoo: &Utf8Path,
) -> Result<()> {
    let portage = sysroot.join("etc/portage");
    std::fs::create_dir_all(&portage).with_context(|| format!("creating {portage}"))?;

    // Materialise an (empty) target package database. Without it the installed
    // loader finds no VDB at `<sysroot>/var/db/pkg` and falls back to the host
    // VDB, so host-installed packages wrongly satisfy target requests and the
    // cross plan comes up empty. An empty dir = "nothing installed in the
    // sysroot yet", which is what we want for a fresh target.
    let vdb = sysroot.join("var/db/pkg");
    std::fs::create_dir_all(&vdb).with_context(|| format!("creating {vdb}"))?;

    write_if_absent(
        &portage.join("make.conf"),
        &make_conf_body(target, sysroot, outer_root),
    )?;

    // Link make.profile DIRECTLY (absolute) to the target-arch profile — eselect
    // profile validates against the host arch and refuses a foreign one.
    let profile_dir = gentoo.join("profiles").join(target.profile_path());
    if !profile_dir.is_dir() {
        bail!(
            "target profile '{}' not found at {profile_dir}",
            target.profile_path()
        );
    }
    symlink_force(&profile_dir, &portage.join("make.profile"))
}

/// Write `<sysroot>/etc/portage/repos.conf` referencing the host gentoo (main)
/// repo and the crossdev overlay, so a cross build with
/// `PORTAGE_CONFIGROOT=<sysroot>` still sees the ebuild tree — the sysroot has no
/// repos of its own (crossdev-stages copies the host `repos.conf` likewise).
fn write_sysroot_repos_conf(
    sysroot: &Utf8Path,
    gentoo: &Utf8Path,
    target: &CrossTarget,
    category: &str,
) -> Result<()> {
    let dir = sysroot.join("etc/portage/repos.conf");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {dir}"))?;
    write_if_absent(
        &dir.join("gentoo.conf"),
        &format!("[DEFAULT]\nmain-repo = gentoo\n\n[gentoo]\nlocation = {gentoo}\n"),
    )?;
    let packages_line = alias_packages_line(target);
    write_if_absent(
        &dir.join(format!("{OVERLAY_NAME}.conf")),
        &format!(
            "[{OVERLAY_NAME}]\nalias-source = gentoo\nalias-target = {category}\n\
             alias-packages = {packages_line}\n"
        ),
    )
}

/// The special cross `make.conf` body (crossdev `set_metadata`): `CHOST`/`CBUILD`
/// so the cross context is detectable, `ARCH`/keywords + target `CFLAGS`. `ROOT`
/// tracks the actual sysroot so a retargeted prefix (`--local`/`--prefix`, e.g.
/// `~/.gentoo/usr/<CTARGET>`) is honoured, not the hardcoded `/usr/<CTARGET>`.
///
/// Deliberately no `CTARGET` here — real crossdev's own target template
/// (`/usr/share/crossdev/etc/portage/make.conf`) never sets it either. `CTARGET`
/// only applies to the host-side `cross-<CTARGET>/{binutils,gcc,...}` builds
/// (`toolchain.eclass` reads it off `CATEGORY`, scoped via [`write_cross_env`]'s
/// `package.env`); leaking it into the sysroot-wide make.conf makes `econf` pass
/// `--target=` to *every* ordinary package, which custom (non-autoconf)
/// `configure` scripts like sqlite's reject outright.
///
/// `MAKEOPTS` mirrors the host's (like `setup::host_makeopts`, for the same
/// reason): without it, every `sys-*` package resolved against this sysroot
/// (`sys-devel/gcc` included) builds fully serial — this make.conf is the
/// *only* one they read, so there is no other source for build parallelism.
/// Caught live: a real stage1 build ran with a single `cc1plus` at a time on a
/// 128-core host because this was missing.
fn make_conf_body(target: &CrossTarget, sysroot: &Utf8Path, outer_root: &Utf8Path) -> String {
    let arch = target.gentoo_arch();
    let tuple = &target.tuple;
    let cbuild = host_chost();
    let makeopts = crate::setup::host_makeopts();
    format!(
        "# Autogenerated by `em crossdev` — cross sysroot for {tuple}.\n\
         CBUILD={cbuild}\n\
         CHOST={tuple}\n\
         ARCH=\"{arch}\"\n\
         ACCEPT_KEYWORDS=\"{arch} ~{arch}\"\n\
         ROOT=\"/\"\n\
         MAKEOPTS=\"{makeopts}\"\n\
         CFLAGS=\"{}\"\n\
         CXXFLAGS=\"${{CFLAGS}}\"\n\
         # The sysroot's own .pc files record paths as if it were \"/\" (e.g.\n\
         # `prefix=/usr`, not the host-absolute sysroot path) — PKG_CONFIG_SYSROOT_DIR\n\
         # prepends the real path onto whatever a .pc reports. PKG_CONFIG_LIBDIR\n\
         # (unlike _PATH) *replaces* pkg-config's default search list, so the host's\n\
         # own .pc files never leak into a foreign-arch cross build (found live:\n\
         # iproute2's ./configure auto-detected the host's net-libs/libtirpc via\n\
         # plain pkg-config, linked -ltirpc, then failed since the target sysroot\n\
         # never had it — see todo/stage-build-shakeout.md).\n\
         PKG_CONFIG_SYSROOT_DIR=\"{sysroot}\"\n\
         PKG_CONFIG_LIBDIR=\"{sysroot}/usr/lib64/pkgconfig:{sysroot}/usr/lib/pkgconfig:{sysroot}/usr/share/pkgconfig\"\n\
         # meson.eclass (and any buildsystem following the same convention) reads\n\
         # BUILD_PKG_CONFIG_LIBDIR for its *native* build-machine pkg-config search\n\
         # path, falling back to the target PKG_CONFIG_LIBDIR above when unset — the\n\
         # same host/target conflation bug as the bare zstd.m4 case in\n\
         # sys-devel/binutils (see todo/stage-build-shakeout.md #29), just for\n\
         # buildsystems that otherwise do the right thing. Point it at the outer\n\
         # EROOT's own native pkgconfig dirs (host BDEPEND packages build there —\n\
         # see [[em-root-characterization]] Tier 1 item 2), not the bare host `/`.\n\
         BUILD_PKG_CONFIG_LIBDIR=\"{outer_root}/usr/lib64/pkgconfig:{outer_root}/usr/lib/pkgconfig:{outer_root}/usr/share/pkgconfig\"\n",
        target.cflags(),
    )
}

/// Write the cross packages' `package.env` + `env/<category>/<pkg>.conf` into the
/// config root's `etc/portage` (where the host-side `cross-*` builds read it).
///
/// Each env file carries the collision-safety crossdev sets on every cross
/// package (`SYMLINK_LIB=no`, a `COLLISION_IGNORE` for the build-id tree) plus
/// the per-ABI multilib block from [`multilib`] (crossdev's `load_multilib_env`):
/// the target ABI's `CFLAGS_<abi>` (`-mabi=lp64d -march=rv64gc`) is what lets the
/// libc build for `<CTARGET>` instead of inheriting the host CFLAGS. em owns these
/// generated files (like crossdev, which regenerates them each run), so they are
/// rewritten rather than preserved.
fn write_cross_env(target: &CrossTarget, globals: &Cli, gentoo: &Utf8Path) -> Result<()> {
    let eclass_dir = gentoo.join("eclass");
    let host_ml = multilib::query(&host_chost(), &eclass_dir)?;
    let target_ml = multilib::query(&target.tuple, &eclass_dir)?;

    let header = format!(
        "CTARGET={}\nSYMLINK_LIB=no\nCOLLISION_IGNORE=\"${{COLLISION_IGNORE}} /usr/lib/debug/.build-id\"\n",
        target.tuple
    );

    // Write into the outer EROOT's `etc/portage`, where the `cross-<tuple>/*`
    // builds read config (the staged driver routes them through
    // `outer_roots()` under `bypass_cross_root` — see `emerge.rs`; that's `/`
    // for `--root`/bare, the prefix for `--prefix`/`--local`, never `--target`'s
    // sysroot substitution). These are HOST-arch-built packages (binutils/gcc
    // produce target code, glibc/linux-headers carry target runtime info)
    // managed via package.env so `emerge -u cross-<tuple>/glibc` works —
    // exactly what real crossdev does (`/etc/portage/package.env/cross-<tuple>`).
    // Write into the build config the `cross-<tuple>/*` packages read — the
    // per-target CTARGET/ABI-CFLAGS env files plus the `package.env` mapping
    // that binds them to each cross package. The read path (`env_files_for`,
    // `ebuild.rs`) consults the config overlay *on top of* the config root, so
    // we write into the overlay when one exists (`--prefix`/`--local`: the
    // user-writable `<prefix>/etc/portage`, avoiding a privileged write to host
    // `/etc/portage`), and fall back to the bare config root otherwise
    // (`--root`/plain host). This keeps the cross env scoped to the prefix and
    // unprivileged, and is read back correctly in every mode — including
    // `bypass_cross_root` toolchain steps, whose `outer_roots()` preserves the
    // same `config_overlay`.
    let base = globals.base_roots();
    let portage = if let Some(overlay) = base.config_overlay() {
        overlay.to_owned()
    } else {
        base.merge_root().join("etc/portage")
    };
    let category = target.category();

    let env_dir = portage.join("env").join(&category);
    std::fs::create_dir_all(&env_dir).with_context(|| format!("creating {env_dir}"))?;

    let mut mappings = String::new();
    // Host-arch tools (binutils/gcc/clang-crossdev-wrappers/gdb — see
    // `PackageArch` on `CrossTarget::packages`) run *on* the build host, not
    // the target, even though they live in the target-influenced
    // `cross-<tuple>` category. Their own keyword acceptance must never
    // depend on whichever arch happens to be active for a given invocation
    // (the sysroot's target arch, under `--target`, vs the bare host arch
    // otherwise) -- found live 2026-07-09: a newer `cross-<tuple>/gcc`
    // resolved fine under `--target` (the generated sysroot make.conf's own
    // `ACCEPT_KEYWORDS="{arch} ~{arch}"` happens to cover it) but failed
    // outright without `--target` (the bare host's real, normally
    // stable-only ACCEPT_KEYWORDS does not). `**` ("accept regardless of
    // keywords" -- portage's own escape hatch, `AcceptToken::Any` in
    // `query/depgraph/repo.rs`) is the correct fix: these tools always run
    // on the host, so no keyword/arch check makes sense for them at all,
    // matching how real crossdev packages are treated.
    let mut keyword_entries = String::new();
    for (_, pkg, arch) in target.packages() {
        let body = format!(
            "{header}{}",
            multilib::env_block(&host_ml, &target_ml, arch.is_target())
        );
        let conf = env_dir.join(format!("{pkg}.conf"));
        std::fs::write(&conf, &body).with_context(|| format!("writing {conf}"))?;
        mappings.push_str(&format!("{category}/{pkg} {category}/{pkg}.conf\n"));
        if arch == target::PackageArch::Host {
            keyword_entries.push_str(&format!("{category}/{pkg} **\n"));
        }
    }

    let pe_dir = portage.join("package.env");
    std::fs::create_dir_all(&pe_dir).with_context(|| format!("creating {pe_dir}"))?;
    let pe = pe_dir.join(&category);
    std::fs::write(&pe, &mappings).with_context(|| format!("writing {pe}"))?;

    let ak_dir = portage.join("package.accept_keywords");
    std::fs::create_dir_all(&ak_dir).with_context(|| format!("creating {ak_dir}"))?;
    let ak = ak_dir.join(&category);
    std::fs::write(&ak, &keyword_entries).with_context(|| format!("writing {ak}"))
}

/// Create the ABI osdir compatibility symlinks the libc leaves out, so the cross
/// gcc finds the target CRT/libc.
///
/// `multilib.eclass` gives the **default ABI** the *un-suffixed* libdir (riscv
/// `LIBDIR_lp64d=lib64`, vs non-default `lp64 → lib64/lp64`), and glibc installs
/// its CRTs/`libc.so` straight into that bare `lib64`. But gcc searches the
/// ABI-suffixed osdir (`lib64/lp64d`), so without a bridge `<CTARGET>-gcc` (and
/// the gcc-stage2 self-build) fails with `cannot find Scrt1.o`. A real crossdev
/// sysroot carries `lib64/lp64d -> .` (and `usr/lib64/lp64d -> .`) — untracked
/// compat symlinks no package owns; em creates them here after the libc lands.
fn link_abi_osdirs(target: &CrossTarget, globals: &Cli) -> Result<()> {
    let sysroot = sysroot(target, globals);
    let gentoo = main_repo(globals)?;
    let ml = multilib::query(&target.tuple, &gentoo.path().join("eclass"))?;
    let default_abi = ml.default_abi();
    // Only the default ABI is bare-named (`lib64` rather than `lib64/<abi>`);
    // the others already install into their suffixed osdir, which gcc finds.
    let Some(libdir) = ml.libdir(default_abi) else {
        return Ok(());
    };
    if libdir.contains('/') || default_abi.is_empty() || default_abi == "default" {
        return Ok(());
    }
    for base in [sysroot.clone(), sysroot.join("usr")] {
        let dir = base.join(libdir);
        if !dir.is_dir() {
            continue;
        }
        let link = dir.join(default_abi);
        symlink_force(Utf8Path::new("."), &link)?;
        println!("    osdir compat: {link} -> .");
    }
    Ok(())
}

/// The host `CHOST` (= the target's `CBUILD`), read from the host `make.conf`.
fn host_chost() -> String {
    MakeConf::load_default()
        .ok()
        .and_then(|m| m.get("CHOST").map(str::to_owned))
        .unwrap_or_else(|| "unknown-host".to_owned())
}

/// Replace whatever is at `link` with a symlink to `dst` (absolute target, so it
/// resolves the same from a sysroot offset).
fn symlink_force(dst: &Utf8Path, link: &Utf8Path) -> Result<()> {
    match std::fs::symlink_metadata(link) {
        Ok(_) => std::fs::remove_file(link).with_context(|| format!("removing {link}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("stat {link}")),
    }
    std::os::unix::fs::symlink(dst, link).with_context(|| format!("linking {link} -> {dst}"))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    fn crossdev_args(show_target_cfg: bool) -> CrossdevArgs {
        CrossdevArgs {
            llvm: false,
            init_target: false,
            setup: false,
            show_target_cfg,
            depgraph_flags: crate::cli::DepgraphFlags::default(),
            merge_flags: crate::cli::MergeFlags::default(),
        }
    }

    /// `--target` is global: `em --target T crossdev --show-target-cfg`
    /// reads it straight off `Cli`. One flag for both "set up" and "use" —
    /// no local `-t` to disagree with it. `--show-target-cfg` only prints
    /// (no filesystem writes), so `run()` is safe to exercise directly here.
    #[tokio::test]
    async fn run_reads_the_global_target() {
        let cli = crate::cli::Cli::parse_from([
            "em",
            "--target",
            "riscv64-unknown-linux-gnu",
            "crossdev",
            "--show-target-cfg",
        ]);
        let args = crossdev_args(true);
        let result = run(&args, &cli).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    /// Neither given: a clear error, not a panic or a silent bare-host guess.
    #[tokio::test]
    async fn run_without_target_is_an_error() {
        let cli = crate::cli::Cli::parse_from(["em", "crossdev", "--show-target-cfg"]);
        let args = crossdev_args(true);
        assert!(run(&args, &cli).await.is_err());
    }

    #[test]
    fn alias_packages_line_is_the_real_cpns_in_stage_order() {
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let line = alias_packages_line(&target);
        // Every token is a real ::gentoo cpn, in packages() order, no cross
        // category, no version — pure derivation source for Location::Alias.
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let expected: Vec<String> = target
            .packages()
            .into_iter()
            .map(|(c, p, _)| format!("{c}/{p}"))
            .collect();
        assert_eq!(
            tokens,
            expected.iter().map(|s| s.as_str()).collect::<Vec<_>>()
        );
        // Every token round-trips through Cpn::parse (the repos.conf reader
        // re-parses these, so an unparseable token would silently drop a
        // package from the derivation map).
        for tok in &tokens {
            assert!(
                portage_atom::Cpn::parse(tok).is_ok(),
                "alias-packages token {tok:?} is not a valid Cpn"
            );
        }
        assert!(!tokens.contains(&"sys-devel/gcc") || line.contains("sys-devel/gcc"));
    }

    /// `write_alias_repo_conf` emits a `Location::Alias` repos.conf entry that
    /// (a) parses back into the expected alias declaration, (b) is idempotent
    /// across re-runs with the same target, and (c) rejects a missing source
    /// package up front with a clear error. Covers the producer half of
    /// derive-on-the-fly in isolation from the prefix-bootstrap topology.
    #[test]
    fn write_alias_repo_conf_emits_a_parseable_alias_entry() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let conf = root.join("etc/portage/repos.conf");
        let gentoo = root.join("gentoo");
        // Skeleton ::gentoo with just the source packages' dirs present.
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let category = target.category();
        for (cat, pkg, _) in target.packages() {
            std::fs::create_dir_all(gentoo.join(cat).join(pkg)).unwrap();
        }
        let globals = test_cli_at_root(root);

        write_alias_repo_conf(&globals, &gentoo, &target, &category).unwrap();
        let file = conf.join(format!("{OVERLAY_NAME}.conf"));
        let body = std::fs::read_to_string(&file).unwrap();
        assert!(body.contains("alias-source = gentoo"));
        assert!(body.contains(&format!("alias-target = {category}")));
        assert!(body.contains("alias-packages = "));

        // Parses back into a Location::Alias with the full package set.
        let rc = portage_repo::ReposConf::load_from(std::slice::from_ref(&conf)).unwrap();
        let entry = rc.find(OVERLAY_NAME).expect("crossdev entry present");
        let portage_repo::Location::Alias { source, aliases } = &entry.location else {
            panic!("expected Location::Alias, got {:?}", entry.location);
        };
        assert_eq!(source, "gentoo");
        let pkgs = aliases
            .get(&category)
            .expect("alias target category present");
        let got: std::collections::HashSet<String> = pkgs.iter().map(|c| c.to_string()).collect();
        for (cat, pkg, _) in target.packages() {
            assert!(
                got.contains(&format!("{cat}/{pkg}")),
                "{cat}/{pkg} missing from parsed alias set {got:?}"
            );
        }

        // Idempotent: a second run with the same target doesn't rewrite, and
        // a divergent-target run (different alias-packages) is tolerated.
        let body_before = std::fs::read_to_string(&file).unwrap();
        write_alias_repo_conf(&globals, &gentoo, &target, &category).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), body_before);
    }

    /// A source package missing from ::gentoo is rejected before any alias is
    /// written — the producer never declares a derivation it can't satisfy.
    #[test]
    fn write_alias_repo_conf_rejects_a_missing_source_package() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path()).unwrap();
        let gentoo = root.join("gentoo");
        // Empty ::gentoo: none of the source packages exist.
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let category = target.category();
        let globals = test_cli_at_root(root);
        let err = write_alias_repo_conf(&globals, &gentoo, &target, &category)
            .expect_err("missing source package rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("not found") && msg.contains(&category),
            "error should name the cross category and missing source: {msg}"
        );
    }

    /// Build a `Cli` whose roots resolve under `root`, so `setup_root`/config
    /// helpers used by the writer land inside the tempdir.
    fn test_cli_at_root(root: &camino::Utf8Path) -> Cli {
        use clap::Parser;
        // `--config-root` scopes both config reads and `setup_root` writes.
        Cli::parse_from([
            "em",
            "--config-root",
            root.as_str(),
            "--root",
            root.as_str(),
        ])
    }

    #[test]
    fn gcc_needs_refresh_cases() {
        // Nothing activated yet: always needs building.
        assert!(gcc_needs_refresh(None, "16"));
        // Older active slot: needs a refresh.
        assert!(gcc_needs_refresh(Some("15"), "16"));
        // Matching or newer active slot: fine as-is.
        assert!(!gcc_needs_refresh(Some("16"), "16"));
        assert!(!gcc_needs_refresh(Some("17"), "16"));
        // Unparseable slots: can't tell, don't force a rebuild.
        assert!(!gcc_needs_refresh(Some("not-a-number"), "16"));
        assert!(!gcc_needs_refresh(Some("15"), "not-a-number"));
    }

    #[test]
    fn atom_is_package_matches_bare_and_version_pinned_atoms() {
        // Bare atom, as toolchain_plan's own gcc-stage1/gcc-stage2 use.
        assert!(atom_is_package(
            "cross-riscv64-unknown-linux-gnu/gcc",
            "gcc"
        ));
        // Version-pinned atom, as gcc_refresh_plan uses to force an exact
        // upgrade — the bug this test guards: a bare `ends_with("/gcc")`
        // check misses this form entirely, silently skipping activation of
        // the freshly-built compiler.
        assert!(atom_is_package(
            "=cross-riscv64-unknown-linux-gnu/gcc-16.1.1_p20260606",
            "gcc"
        ));
        // Doesn't false-positive on an unrelated package with a shared prefix.
        assert!(!atom_is_package(
            "cross-riscv64-unknown-linux-gnu/gcc-doc",
            "gcc"
        ));
        assert!(!atom_is_package("sys-devel/binutils", "gcc"));
    }

    /// The sysroot-wide `make.conf` must never set `CTARGET`: unlike real
    /// crossdev, which scopes it via `package.env` to the host-side
    /// `cross-<CTARGET>/{binutils,gcc,...}` builds only, a sysroot-wide
    /// `CTARGET` leaks into every ordinary package's `econf` invocation
    /// (`--target=`), which non-autoconf `configure` scripts (e.g. sqlite's)
    /// reject outright.
    #[test]
    fn make_conf_body_never_sets_ctarget() {
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let body = make_conf_body(
            &target,
            Utf8Path::new("/usr/riscv64-unknown-linux-gnu"),
            Utf8Path::new("/"),
        );
        assert!(
            !body.lines().any(|l| l.starts_with("CTARGET=")),
            "sysroot make.conf must not set CTARGET:\n{body}"
        );
        assert!(body.contains("CHOST=riscv64-unknown-linux-gnu"));
    }

    /// The sysroot make.conf is the *only* config `sys-devel/gcc` and every
    /// other ordinary stage1 package resolved against `--target` ever reads —
    /// unlike the self-contained `--root`'s own make.conf
    /// (`setup::host_makeopts`'s doc comment), there is no fallback host
    /// config to inherit build parallelism from. Missing this made a real
    /// stage1 build run fully serial (one `cc1plus` at a time on a 128-core
    /// host) — caught live while chasing an unrelated gcc version-mismatch
    /// bug, the same class of gap as `self_contained_root_gets_real_makeopts`
    /// in `setup.rs`.
    #[test]
    fn make_conf_body_sets_makeopts() {
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let body = make_conf_body(
            &target,
            Utf8Path::new("/usr/riscv64-unknown-linux-gnu"),
            Utf8Path::new("/"),
        );
        assert!(body.contains("MAKEOPTS="), "sysroot make.conf:\n{body}");
        assert!(
            !body.contains("MAKEOPTS=\"\""),
            "must be non-empty:\n{body}"
        );
    }

    /// Regression test for the iproute2 stage3 failure: `./configure` ran
    /// plain `pkg-config`, found the *host's* `net-libs/libtirpc.pc`
    /// (`net-libs/libtirpc` isn't even in DEPEND — USE=-nfs — let alone
    /// installed in the target sysroot), and linked `-ltirpc` into a build
    /// that then failed since the library genuinely isn't in the sysroot.
    /// `PKG_CONFIG_SYSROOT_DIR`/`PKG_CONFIG_LIBDIR` must scope pkg-config to
    /// the sysroot so a foreign-arch cross build never sees host `.pc` files.
    #[test]
    fn make_conf_body_scopes_pkg_config_to_the_sysroot() {
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let sysroot = "/var/tmp/cross-stage1-riscv64/usr/riscv64-unknown-linux-gnu";
        let body = make_conf_body(
            &target,
            Utf8Path::new(sysroot),
            Utf8Path::new("/var/tmp/cross-stage1-riscv64"),
        );
        assert!(
            body.contains(&format!("PKG_CONFIG_SYSROOT_DIR=\"{sysroot}\"")),
            "sysroot make.conf:\n{body}"
        );
        assert!(
            body.contains("PKG_CONFIG_LIBDIR=")
                && body.contains(&format!("{sysroot}/usr/lib64/pkgconfig"))
                && body.contains(&format!("{sysroot}/usr/share/pkgconfig")),
            "PKG_CONFIG_LIBDIR must point into the sysroot only:\n{body}"
        );
        // PKG_CONFIG_LIBDIR *replaces* the default search list — the whole
        // point is that no host pkgconfig dir leaks in.
        assert!(
            !body.contains("PKG_CONFIG_PATH="),
            "must not additively leak the host's pkgconfig search path:\n{body}"
        );
    }

    /// meson.eclass (and any buildsystem following the same convention) reads
    /// `BUILD_PKG_CONFIG_LIBDIR` for its native build-machine pkg-config
    /// search path, falling back to the *target* `PKG_CONFIG_LIBDIR` when
    /// unset — the same host/target conflation that broke
    /// `sys-devel/binutils`'s bare `zstd.m4` check (#29), just for
    /// buildsystems that otherwise get this right. It must point at the
    /// outer EROOT (where Host BDEPEND packages actually build — see
    /// `entry_roots()` in `main.rs`), not the target sysroot and not the
    /// bare host `/`.
    #[test]
    fn make_conf_body_sets_build_pkg_config_libdir_to_the_outer_root() {
        let target = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let sysroot = "/var/tmp/cross-stage1-riscv64/usr/riscv64-unknown-linux-gnu";
        let outer_root = "/var/tmp/cross-stage1-riscv64";
        let body = make_conf_body(&target, Utf8Path::new(sysroot), Utf8Path::new(outer_root));
        assert!(
            body.contains(&format!(
                "BUILD_PKG_CONFIG_LIBDIR=\"{outer_root}/usr/lib64/pkgconfig"
            )),
            "BUILD_PKG_CONFIG_LIBDIR must point into the outer EROOT:\n{body}"
        );
        assert!(
            !body.contains(&format!("BUILD_PKG_CONFIG_LIBDIR=\"{sysroot}")),
            "BUILD_PKG_CONFIG_LIBDIR must not point into the target sysroot:\n{body}"
        );
    }
}
