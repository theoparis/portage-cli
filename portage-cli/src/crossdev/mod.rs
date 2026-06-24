//! `em crossdev` — set up a cross-compilation target, a `crossdev` workalike.
//!
//! Implements the **no-build setup** (`--init-target` / `--show-target-cfg`):
//! overlay creation (the `cross-*` symlink category + `metadata`/`profiles` + a
//! `repos.conf` entry), the cross sysroot `make.conf`, and the **direct**
//! `make.profile` symlink (`eselect profile` refuses a foreign arch). `--setup`
//! additionally derives the ordered [`stages::toolchain_plan`] bootstrap
//! (binutils → headers → gcc-stage1 → libc → gcc-stage2); driving each step
//! through the merge path is the remaining work (see todo/crossdev-target.md).
//!
//! The install location follows em's root model: the sysroot is
//! `<EROOT>/usr/<CTARGET>`, so `em crossdev <t>` targets `/usr/<CTARGET>` (like
//! crossdev), `em --local crossdev <t>` targets `~/.gentoo/usr/<CTARGET>`, and
//! `em --prefix DIR`/`--root DIR` retarget under `DIR`.

mod stages;
mod target;

use std::io::Write;

use anyhow::{Context, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use portage_repo::{MakeConf, ReposConf, Repository};

use crate::cli::{Cli, CrossdevArgs, DepgraphFlags};
use crate::style::{C_LABEL, C_PKG};
use crate::util::write_if_absent;
use target::CrossTarget;

/// Merge depgraph flags from args into globals, with args taking precedence.
fn merge_depgraph_flags(globals: &Cli, args: &CrossdevArgs) -> DepgraphFlags {
    DepgraphFlags {
        deep: args.depgraph_flags.deep || globals.depgraph_flags.deep,
        newuse: args.depgraph_flags.newuse || globals.depgraph_flags.newuse,
    }
}

/// The overlay name crossdev uses — one overlay holds every `cross-*` category.
const OVERLAY_NAME: &str = "crossdev";

pub async fn run(args: &CrossdevArgs, globals: &Cli) -> Result<()> {
    let target = CrossTarget::parse(&args.target, args.llvm)?;

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
    let plan = stages::toolchain_plan(target);
    let mut out = anstream::stdout();
    let verb = if globals.pretend { "Plan" } else { "Bootstrap" };
    writeln!(
        out,
        "\n{C_LABEL}{verb} cross toolchain{C_LABEL:#} ({}) — {} steps:",
        target.tuple,
        plan.steps.len()
    )
    .ok();

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
        let merged_flags = merge_depgraph_flags(globals, args);
        crate::emerge_atoms(
            globals,
            &step.atoms,
            crate::EmergeOpts {
                use_override: &step.use_override,
                nodeps: step.nodeps,
                depgraph_flags: Some(merged_flags.clone()),
            },
        )
        .await?;
    }

    if !globals.pretend {
        writeln!(
            out,
            "\n>>> cross toolchain {} ready in {}/usr/{}",
            target.tuple,
            globals.roots().merge_root(),
            target.tuple,
        )
        .ok();
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

/// `EROOT`/prefix the overlay, `repos.conf`, and `package.env` are written under
/// (`~/.gentoo` for `--local`), so an unprivileged setup is writable + readable.
fn setup_root(globals: &Cli) -> Utf8PathBuf {
    globals.roots().merge_root().to_owned()
}

/// The target sysroot `<EROOT>/usr/<CTARGET>` (EROOT = `/` by default, the prefix
/// for `--local`/`--prefix`, the root for `--root`).
fn sysroot(target: &CrossTarget, globals: &Cli) -> Utf8PathBuf {
    globals.roots().merge_root().join("usr").join(&target.tuple)
}

/// The configured main repo (`gentoo`) — the real ebuilds the overlay links to.
fn main_repo(globals: &Cli) -> Result<Repository> {
    let conf = globals.roots().repos_conf().context("reading repos.conf")?;
    let entry = conf
        .main_repo()
        .or_else(|| conf.find("gentoo"))
        .context("no main repo configured in repos.conf")?;
    Repository::open(&entry.location)
        .with_context(|| format!("opening main repo at {}", entry.location.display()))
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
    for (cat, pkg) in target.packages() {
        writeln!(out, "    {C_PKG}{category}/{pkg}{C_PKG:#} → {cat}/{pkg}").ok();
    }
    Ok(())
}

fn init_target(target: &CrossTarget, globals: &Cli) -> Result<()> {
    let gentoo = main_repo(globals)?;
    let gentoo_path = gentoo.path().to_owned();
    let overlay = setup_root(globals).join("var/db/repos").join(OVERLAY_NAME);
    let sysroot = sysroot(target, globals);

    write_overlay(target, &overlay, &gentoo_path)?;
    write_cross_env(target, globals)?;
    ensure_repos_conf(globals, &overlay)?;
    write_sysroot_config(target, &sysroot, &gentoo_path)?;
    write_sysroot_repos_conf(&sysroot, &gentoo_path, &overlay)?;

    println!(">>> cross target {} ready", target.tuple);
    println!("    overlay:  {overlay}  ({})", target.category());
    println!("    sysroot:  {sysroot}");
    // The toolchain itself is a HOST build (compiler lands on /), so it resolves
    // with host config — NOT the sysroot (that fights the cross make.conf ROOT).
    println!(
        "    toolchain: em -p {}/gcc          # host build of the cross compiler",
        target.category()
    );
    Ok(())
}

/// Lay down the overlay: `metadata/layout.conf`, `profiles/{repo_name,categories}`,
/// and the `cross-*` category of per-package symlinks into `::gentoo`.
fn write_overlay(target: &CrossTarget, overlay: &Utf8Path, gentoo: &Utf8Path) -> Result<()> {
    let meta = overlay.join("metadata");
    let profiles = overlay.join("profiles");
    std::fs::create_dir_all(&meta).with_context(|| format!("creating {meta}"))?;
    std::fs::create_dir_all(&profiles).with_context(|| format!("creating {profiles}"))?;

    write_if_absent(
        &meta.join("layout.conf"),
        "masters = gentoo\nthin-manifests = true\nsign-manifests = false\n",
    )?;
    write_if_absent(&profiles.join("repo_name"), &format!("{OVERLAY_NAME}\n"))?;

    let category = target.category();
    append_line(&profiles.join("categories"), &category)?;

    let cat_dir = overlay.join(&category);
    std::fs::create_dir_all(&cat_dir).with_context(|| format!("creating {cat_dir}"))?;
    for (real_cat, pkg) in target.packages() {
        let dst = gentoo.join(real_cat).join(pkg);
        if !dst.is_dir() {
            bail!("{real_cat}/{pkg} not found at {dst} (needed for {category}/{pkg})");
        }
        symlink_force(&dst, &cat_dir.join(pkg))?;
    }
    Ok(())
}

/// Register the overlay in `repos.conf` if no entry of that name exists yet
/// (crossdev/eselect may already provide one — don't duplicate it).
fn ensure_repos_conf(globals: &Cli, overlay: &Utf8Path) -> Result<()> {
    let conf_dir = setup_root(globals).join("etc/portage/repos.conf");
    if let Ok(conf) = ReposConf::load_from(&[&conf_dir])
        && conf.find(OVERLAY_NAME).is_some()
    {
        return Ok(());
    }
    let dir = conf_dir;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {dir}"))?;
    write_if_absent(
        &dir.join(format!("{OVERLAY_NAME}.conf")),
        &format!("[{OVERLAY_NAME}]\nlocation = {overlay}\nmasters = gentoo\nauto-sync = false\n"),
    )
}

/// Write the cross sysroot `etc/portage/{make.conf,make.profile}`.
fn write_sysroot_config(target: &CrossTarget, sysroot: &Utf8Path, gentoo: &Utf8Path) -> Result<()> {
    let portage = sysroot.join("etc/portage");
    std::fs::create_dir_all(&portage).with_context(|| format!("creating {portage}"))?;

    // Materialise an (empty) target package database. Without it the installed
    // loader finds no VDB at `<sysroot>/var/db/pkg` and falls back to the host
    // VDB, so host-installed packages wrongly satisfy target requests and the
    // cross plan comes up empty. An empty dir = "nothing installed in the
    // sysroot yet", which is what we want for a fresh target.
    let vdb = sysroot.join("var/db/pkg");
    std::fs::create_dir_all(&vdb).with_context(|| format!("creating {vdb}"))?;

    write_if_absent(&portage.join("make.conf"), &make_conf_body(target, sysroot))?;

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
    overlay: &Utf8Path,
) -> Result<()> {
    let dir = sysroot.join("etc/portage/repos.conf");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {dir}"))?;
    write_if_absent(
        &dir.join("gentoo.conf"),
        &format!("[DEFAULT]\nmain-repo = gentoo\n\n[gentoo]\nlocation = {gentoo}\n"),
    )?;
    write_if_absent(
        &dir.join(format!("{OVERLAY_NAME}.conf")),
        &format!("[{OVERLAY_NAME}]\nlocation = {overlay}\nmasters = gentoo\nauto-sync = false\n"),
    )
}

/// The special cross `make.conf` body (crossdev `set_metadata`): `CHOST`/`CBUILD`
/// so the cross context is detectable, `ARCH`/keywords + target `CFLAGS`. `ROOT`
/// tracks the actual sysroot so a retargeted prefix (`--local`/`--prefix`, e.g.
/// `~/.gentoo/usr/<CTARGET>`) is honoured, not the hardcoded `/usr/<CTARGET>`.
fn make_conf_body(target: &CrossTarget, sysroot: &Utf8Path) -> String {
    let arch = target.gentoo_arch();
    let tuple = &target.tuple;
    let cbuild = host_chost();
    format!(
        "# Autogenerated by `em crossdev` — cross sysroot for {tuple}.\n\
         CBUILD={cbuild}\n\
         CHOST={tuple}\n\
         CTARGET={tuple}\n\
         ARCH=\"{arch}\"\n\
         ACCEPT_KEYWORDS=\"{arch} ~{arch}\"\n\
         ROOT=\"{sysroot}/\"\n\
         CFLAGS=\"{}\"\n\
         CXXFLAGS=\"${{CFLAGS}}\"\n",
        target.cflags(),
    )
}

/// Write the cross packages' `package.env` + `env/<category>/<pkg>.conf` into the
/// config root's `etc/portage` (where the host-side `cross-*` builds read it).
///
/// Each env file carries the collision-safety crossdev sets on every cross
/// package: `SYMLINK_LIB=no` and a `COLLISION_IGNORE` for the build-id tree, so
/// several cross toolchains can coexist on one host. The full per-ABI multilib
/// block crossdev's `load_multilib_env` emits (CHOST_*/LIBDIR_*/ABI/…) is
/// arch-specific and deferred to the build stages.
fn write_cross_env(target: &CrossTarget, globals: &Cli) -> Result<()> {
    const ENV_HEADER: &str =
        "SYMLINK_LIB=no\nCOLLISION_IGNORE=\"${COLLISION_IGNORE} /usr/lib/debug/.build-id\"\n";

    let portage = setup_root(globals).join("etc/portage");
    let category = target.category();

    let env_dir = portage.join("env").join(&category);
    std::fs::create_dir_all(&env_dir).with_context(|| format!("creating {env_dir}"))?;

    let mut mappings = String::new();
    for (_, pkg) in target.packages() {
        write_if_absent(&env_dir.join(format!("{pkg}.conf")), ENV_HEADER)?;
        mappings.push_str(&format!("{category}/{pkg} {category}/{pkg}.conf\n"));
    }

    let pe_dir = portage.join("package.env");
    std::fs::create_dir_all(&pe_dir).with_context(|| format!("creating {pe_dir}"))?;
    write_if_absent(&pe_dir.join(&category), &mappings)
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

/// Append `line` to `path` (one per line), creating it if absent, skipping if the
/// exact line is already present.
fn append_line(path: &Utf8Path, line: &str) -> Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    if existing.lines().any(|l| l == line) {
        return Ok(());
    }
    let mut body = existing;
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(line);
    body.push('\n');
    std::fs::write(path, body).with_context(|| format!("writing {path}"))
}
