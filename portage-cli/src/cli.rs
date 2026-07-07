use std::str::FromStr;

use clap::builder::styling::{AnsiColor as ClapAnsiColor, Styles};
use clap::{Parser, Subcommand};
use gentoo_core::Arch;

mod depgraph_flags;
mod merge_flags;
pub use depgraph_flags::DepgraphFlags;
pub use merge_flags::MergeFlags;

const fn cli_styles() -> Styles {
    Styles::styled()
        .header(ClapAnsiColor::Yellow.on_default().bold())
        .usage(ClapAnsiColor::Green.on_default().bold())
        .literal(ClapAnsiColor::Green.on_default())
        .placeholder(ClapAnsiColor::Cyan.on_default())
        .error(ClapAnsiColor::Red.on_default().bold())
        .valid(ClapAnsiColor::Green.on_default())
        .invalid(ClapAnsiColor::Red.on_default())
}

#[derive(Parser)]
#[command(
    name = "em",
    version,
    about = "Gentoo Portage package manager workalike",
    arg_required_else_help = true,
    styles = cli_styles()
)]
pub struct Cli {
    #[command(flatten)]
    pub color: colorchoice_clap::Color,

    #[command(flatten)]
    pub depgraph_flags: DepgraphFlags,

    /// Show what would be done without actually performing any actions.
    #[arg(short = 'p', long, global = true)]
    pub pretend: bool,

    /// Ask for confirmation before performing actions.
    #[arg(short = 'a', long, global = true)]
    pub ask: bool,

    /// Increase verbosity (can be repeated for more detail).
    #[arg(short = 'v', long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress non-error output.
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    /// Target architecture for operations. Defaults to current system architecture.
    #[arg(long, value_name = "ARCH", default_value_t = Arch::current(), value_parser = parse_arch)]
    pub arch: Arch,

    /// Pin search/query to a single repository. When unset, repositories are
    /// auto-discovered from `repos.conf` (the main repo wins for single-repo
    /// applets; search walks all of them).
    #[arg(long, value_name = "PATH")]
    pub repo: Option<String>,

    /// Unprivileged offset: ROOT/VDB/distfiles/build trees under DIR; config
    /// still from the host (use --root for a config offset).
    #[arg(long, value_name = "DIR", global = true)]
    pub prefix: Option<String>,

    /// Unprivileged in-place Gentoo-Prefix at ~/.gentoo (EPREFIX=~/.gentoo);
    /// config from the host, overlaid by ~/.gentoo/etc/portage.
    #[arg(long, global = true)]
    pub local: bool,

    /// How an unprivileged build gets root for chown/setuid: auto (best
    /// compiled-in fake root), pseudoroot, fakeroost, hakoniwa (userns mapped
    /// root), sudo (real root), or none; backends unsupported on this platform
    /// are compiled out. Ignored when already root.
    #[arg(long, value_enum, default_value_t = Privilege::Auto, global = true, env = "EM_PRIVILEGE")]
    pub privilege: Privilege,

    /// Search package names (each argument is a pattern).
    #[arg(short = 's', long)]
    pub search: bool,

    /// Search package names and descriptions.
    #[arg(short = 'S', long)]
    pub searchdesc: bool,

    /// Skip dependency resolution and only merge specified packages.
    #[arg(short = 'O', long)]
    pub nodeps: bool,

    #[command(flatten)]
    pub merge_flags: MergeFlags,

    /// Installation root (the offset all applets install into / query).
    #[arg(long, env = "ROOT", value_name = "PATH", global = true)]
    pub root: Option<String>,

    /// Read config (profile, make.conf) from this root instead of `--root`.
    #[arg(long, value_name = "PATH", global = true)]
    pub config_root: Option<String>,

    /// Override VDB path (default: $ROOT/var/db/pkg)
    #[arg(long, value_name = "PATH", global = true)]
    pub vdb: Option<String>,

    /// Cross-build for a crossdev target tuple: resolve/install into the target
    /// sysroot `<EROOT>/usr/<TUPLE>` (the crossdev `<TUPLE>-emerge` entry point).
    /// Sugar for `--config-root <sysroot> --root <sysroot>`; the cross context
    /// (CHOST/CBUILD, `--root-deps=rdeps`) is read from the sysroot make.conf.
    /// Set the target up first with `em crossdev -t <TUPLE> --init-target`.
    #[arg(long, value_name = "TUPLE", global = true)]
    pub cross: Option<String>,

    #[command(subcommand)]
    pub applet: Option<Applet>,

    #[arg(num_args = 1..)]
    pub atoms: Vec<String>,
}

/// The resolved set of roots for a command (see docs/root-model.md): config
/// source, the planner's installed base, and the install target. Built once
/// from the global flags via [`Cli::roots`] and passed around as a unit.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    config: Option<camino::Utf8PathBuf>,
    base: Option<camino::Utf8PathBuf>,
    target: Option<camino::Utf8PathBuf>,
    /// `EPREFIX`: when set (`--local`), packages are configured for and
    /// installed in place at this offset (`target == eprefix`, so `EROOT ==
    /// target` and `ROOT == /`). `None` for ROOT-offset / host builds.
    eprefix: Option<camino::Utf8PathBuf>,
    /// A user-writable config dir overlaid on the host config for
    /// `package.use`/`bashrc` (the `~/.gentoo/etc/portage` of `--local`),
    /// so an unprivileged user can override without touching `/etc/portage`.
    config_overlay: Option<camino::Utf8PathBuf>,
    relocate: bool,
}

impl Roots {
    /// `PORTAGE_CONFIGROOT`: where profile and make.conf are read.
    pub fn config(&self) -> Option<&camino::Utf8Path> {
        self.config.as_deref()
    }

    /// The base root whose VDB seeds the planner's "installed" view.
    pub fn base(&self) -> Option<&camino::Utf8Path> {
        self.base.as_deref()
    }

    /// The install target: where new packages land and the delta VDB lives.
    pub fn target(&self) -> Option<&camino::Utf8Path> {
        self.target.as_deref()
    }

    /// The install/merge root (`EROOT`), defaulting to `/`. With `--local`
    /// this is the prefix (`target == eprefix`); files and the VDB land here.
    pub fn merge_root(&self) -> &camino::Utf8Path {
        self.target.as_deref().unwrap_or(camino::Utf8Path::new("/"))
    }

    /// `EPREFIX` for an in-place prefix build (`--local`), else `None`.
    pub fn eprefix(&self) -> Option<&camino::Utf8Path> {
        self.eprefix.as_deref()
    }

    /// Whether this is an overlay view (EPREFIX set, base is the host): the
    /// `--prefix` case where `base_roots()`'s merge_root is the host but the
    /// actual install target is the prefix. `roots()` uses this to reconstruct
    /// the prefix-target view on top of `base_roots()`.
    pub(crate) fn is_overlay(&self) -> bool {
        self.eprefix.is_some() && self.base.is_none()
    }

    /// User config overlay dir (`package.use`/`bashrc` layered on host config).
    pub fn config_overlay(&self) -> Option<&camino::Utf8Path> {
        self.config_overlay.as_deref()
    }

    /// The build-against sysroot (`SYSROOT`/`ESYSROOT`) to hand the shell:
    /// `None` means "same as the install target" (full offset / host), so the
    /// shell defaults `SYSROOT = ROOT`. `Some` only for an overlay where the
    /// base differs from the target (`--prefix`), where the base is the system
    /// to build against and the target is layered on top.
    pub fn build_sysroot(&self) -> Option<&camino::Utf8Path> {
        if self.base.as_deref() != self.target.as_deref() {
            Some(self.base.as_deref().unwrap_or(camino::Utf8Path::new("/")))
        } else {
            None
        }
    }

    /// Whether `--prefix` relocates distfiles and the build trees under the
    /// target (a self-contained tree).
    pub fn relocate(&self) -> bool {
        self.relocate
    }

    /// `ESYSROOT` / cross sysroot: `PORTAGE_CONFIGROOT` when set, else base.
    pub fn sysroot(&self) -> Option<&camino::Utf8Path> {
        self.config.as_deref().or(self.base.as_deref())
    }

    /// Load `repos.conf` portage-style for this invocation: global defaults +
    /// confdir under the config root, plus the `--local`/`--prefix` overlay
    /// confdir. The single source of truth for repo discovery.
    pub fn repos_conf(&self) -> portage_repo::Result<portage_repo::ReposConf> {
        let cfg = self.config().unwrap_or_else(|| camino::Utf8Path::new("/"));
        let extra: Vec<&camino::Utf8Path> = self.config_overlay().into_iter().collect();
        portage_repo::ReposConf::load_rooted(cfg, &extra)
    }

    /// Test-only: a `Roots` with `base` and `target` both set to the same
    /// path (matching a plain `--root DIR` invocation), for exercising
    /// root-selection logic without a full CLI parse and without any VDB
    /// lookup silently falling through to the real bare host's.
    #[cfg(test)]
    pub(crate) fn for_test(target: &str) -> Self {
        let path = camino::Utf8PathBuf::from(target);
        Roots {
            base: Some(path.clone()),
            target: Some(path),
            ..Default::default()
        }
    }
}

/// The user's home directory from `$HOME`, falling back to `/root` only if
/// unset (matching how unprivileged tools resolve `~`).
fn home_dir() -> camino::Utf8PathBuf {
    std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(camino::Utf8PathBuf::from)
        .unwrap_or_else(|| camino::Utf8PathBuf::from("/root"))
}

impl Cli {
    /// Resolve the root model (docs/root-topology.md) from the global flags.
    ///
    /// `--cross <tuple>` layers on top of the base model: it targets the crossdev
    /// sysroot `<EROOT>/usr/<tuple>` as both config-root and root (crossdev's
    /// `PORTAGE_CONFIGROOT == ROOT == SYSROOT`). The `<EROOT>` it sits under still
    /// comes from `--local`/`--prefix`/`--root`, so `em --local --cross <t>`
    /// targets `~/.gentoo/usr/<t>`.
    ///
    /// Under `--prefix`, the returned `Roots`'s `merge_root()` is the **prefix**
    /// (install destination), while `base_roots()` returns a separate view whose
    /// `merge_root()` is the **host `/`** (BROOT, for BDEPEND checks). The two
    /// genuinely differ for an overlay; this split is what lets preflight check
    /// BDEPEND against the host while the merge lands in the prefix.
    pub fn roots(&self) -> Roots {
        // --cross: layer the sysroot on top of the overlay target (the prefix),
        // not base_roots's BROOT (host /). Under --prefix the cross sysroot is
        // <prefix>/usr/<tuple>, and base_roots's merge_root is the host — so
        // derive the sysroot from the overlay's prefix (eprefix) when set.
        let base = self.base_roots();
        let Some(tuple) = self.cross.as_deref() else {
            // No --cross. Under --prefix, base_roots()'s merge_root is the host
            // (BROOT); the actual install target is the prefix. Reconstruct the
            // overlay view here so callers see target=prefix.
            if let Some(prefix) = base.eprefix.as_deref().filter(|_| base.is_overlay()) {
                return Roots {
                    config: base.config.clone(),
                    base: None,
                    target: Some(prefix.to_path_buf()),
                    eprefix: Some(prefix.to_path_buf()),
                    config_overlay: Some(prefix.join("etc/portage")),
                    relocate: true,
                };
            }
            return base;
        };
        // The outer EROOT the sysroot sits under: the overlay prefix when set
        // (--prefix), else base_roots's merge_root (host / for plain --cross).
        let eroot = base
            .eprefix
            .as_deref()
            .map(camino::Utf8PathBuf::from)
            .unwrap_or_else(|| base.merge_root().to_owned());
        let sysroot = eroot.join("usr").join(tuple);
        Roots {
            config: Some(sysroot.clone()),
            base: Some(sysroot.clone()),
            target: Some(sysroot),
            eprefix: None,
            config_overlay: None,
            relocate: false,
        }
    }

    /// The root model from `--local`/`--prefix`/`--root`/`--config-root`, before
    /// any `--cross` sysroot override (see [`roots`](Self::roots)). Exposed at
    /// `pub(crate)` so the staged-build driver can install `cross-*` toolchain
    /// packages (which always live in the outer EROOT, never the sysroot
    /// subdirectory — see `crossdev/mod.rs`'s module doc) even from a
    /// `--cross`-active invocation.
    ///
    /// `merge_root()` of the returned `Roots` is the **BROOT** — where BDEPEND
    /// tools run and are checked against (docs/root-topology.md § "Override
    /// semantics"). Under `--prefix` BROOT is the host `/` (the overlay borrows
    /// host tools); under `--local`/`--root` BROOT is the offset itself
    /// (standalone/self-contained). Under `--cross` BROOT is the outer EROOT
    /// (the sysroot substitution is undone here, applied in `roots()`).
    pub(crate) fn base_roots(&self) -> Roots {
        let path = |s: &Option<String>| s.as_deref().map(camino::Utf8PathBuf::from);
        // `--local`: standalone Gentoo-Prefix at ~/.gentoo. Full closure (base
        // == target == ~/.gentoo), self-contained VDB. EPREFIX makes installed
        // scripts relocatable (shebangs reference ${EPREFIX}/usr/bin/...). The
        // prefix builds its own python via `toolchain --setup`; during bootstrap
        // the host compiler is reached via PATH, never via a symlink masquerading
        // as a prefix-owned file (that's the overlay's job — see --prefix below).
        // See docs/root-topology.md § "Override semantics".
        if self.local {
            let prefix = home_dir().join(".gentoo");
            return Roots {
                config: None,
                base: Some(prefix.clone()),
                target: Some(prefix.clone()),
                eprefix: Some(prefix.clone()),
                config_overlay: Some(prefix.join("etc/portage")),
                relocate: true,
            };
        }
        // `--prefix` overlay: BROOT is the host `/`. The prefix is the install
        // destination (target), but base_roots()'s merge_root() must be the host
        // because that's what preflight/bdepend_avail check BDEPEND against.
        // roots() reconstructs the prefix-target view on top of this.
        if self.prefix.is_some() {
            let prefix = path(&self.prefix).unwrap();
            return Roots {
                config: path(&self.config_root),
                base: None,
                target: None, // BROOT = host `/`, NOT the prefix
                eprefix: Some(prefix.clone()),
                config_overlay: Some(prefix.join("etc/portage")),
                relocate: true,
            };
        }
        Roots {
            // config: --config-root, else --root; host otherwise.
            // (TODO: portage `ROOT=` parity — --root should NOT move config.
            //  Deferred to Cluster B: ensure_self_contained_prefix uses
            //  config().is_some() as a self-contained signal that breaks if
            //  --root stops setting config. See docs/root-topology.md.)
            config: path(&self.config_root).or_else(|| path(&self.root)),
            // base: --root; host otherwise.
            base: path(&self.root),
            // target: --root (install destination). BROOT == target for --root.
            target: path(&self.root),
            eprefix: None,
            config_overlay: None,
            relocate: false,
        }
    }

    /// Path used by single-repo applets. Falls back to `/var/db/repos/gentoo`
    /// when neither `--repo` nor `repos.conf` is available.
    pub fn repo_path(&self) -> String {
        if let Some(p) = &self.repo {
            return p.clone();
        }
        if let Ok(rc) = self.roots().repos_conf()
            && let Some(main) = rc.main_repo()
        {
            return main.location.to_string_lossy().into_owned();
        }
        "/var/db/repos/gentoo".to_string()
    }

    /// Repositories to walk for `em search`. Honours `--repo` when set;
    /// otherwise returns every entry from `repos.conf` (main first).
    pub fn search_repos(&self) -> Vec<std::path::PathBuf> {
        if let Some(p) = &self.repo {
            return vec![std::path::PathBuf::from(p)];
        }
        match self.roots().repos_conf() {
            Ok(rc) if !rc.repos().is_empty() => {
                rc.repos().iter().map(|e| e.location.clone()).collect()
            }
            _ => vec![std::path::PathBuf::from("/var/db/repos/gentoo")],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_targets_sysroot_under_eroot() {
        // `--cross` sits under the `--root` EROOT and pins config == base ==
        // target to `<EROOT>/usr/<tuple>` (PORTAGE_CONFIGROOT == ROOT == SYSROOT).
        let cli = Cli::parse_from([
            "em",
            "--root",
            "/srv/x",
            "--cross",
            "riscv64-unknown-linux-gnu",
            "-p",
            "sys-libs/zlib",
        ]);
        let r = cli.roots();
        let sysroot = "/srv/x/usr/riscv64-unknown-linux-gnu";
        assert_eq!(r.config().unwrap().as_str(), sysroot);
        assert_eq!(r.merge_root().as_str(), sysroot);
        assert_eq!(r.base().unwrap().as_str(), sysroot);
        assert_eq!(r.config(), r.target());
    }

    #[test]
    fn cross_defaults_to_root_eroot() {
        // No `--root`: EROOT is `/`, so the sysroot is `/usr/<tuple>`.
        let cli = Cli::parse_from(["em", "--cross", "riscv64-unknown-linux-gnu", "-p", "zlib"]);
        assert_eq!(
            cli.roots().merge_root().as_str(),
            "/usr/riscv64-unknown-linux-gnu"
        );
    }

    #[test]
    fn no_cross_keeps_base_roots() {
        let cli = Cli::parse_from(["em", "-p", "sys-libs/zlib"]);
        let r = cli.roots();
        assert_eq!(r.config(), None);
        assert_eq!(r.merge_root().as_str(), "/");
    }

    /// `--local` is a standalone prefix: base == target == ~/.gentoo (full
    /// closure, own VDB), not an overlay (base would be the host). Previously
    /// base was None (host) — wrong for cross on a foreign host, where there's
    /// no host VDB to seed the plan. See docs/root-topology.md § "Override
    /// semantics".
    #[test]
    fn local_is_standalone_not_overlay() {
        // HOME is process-global; save/restore to avoid interfering with
        // parallel tests. (Edition 2024 makes set_var unsafe.)
        let saved = std::env::var("HOME").ok();
        // SAFETY: no other thread in this test process touches HOME.
        unsafe {
            std::env::set_var("HOME", "/tmp/fake-home");
        }
        let cli = Cli::parse_from(["em", "--local", "-p", "sys-libs/zlib"]);
        let r = cli.base_roots();
        assert_eq!(
            r.base().unwrap().as_str(),
            "/tmp/fake-home/.gentoo",
            "--local base must be the prefix (standalone), not the host"
        );
        assert_eq!(
            r.base(),
            r.target(),
            "--local base == target (full closure)"
        );
        // Restore.
        unsafe {
            match &saved {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    /// `--prefix` sets EPREFIX: the installed tree is relocatable, so ebuilds
    /// bake ${EPREFIX}/usr/bin/pythonX.Y into shebangs. The overlay then
    /// symlinks host python there (setup.rs) to satisfy them without building
    /// a prefix python. See docs/root-topology.md § "Override semantics".
    #[test]
    fn prefix_sets_eprefix_for_relocatable_overlay() {
        let cli = Cli::parse_from(["em", "--prefix", "/opt/p", "-p", "sys-libs/zlib"]);
        let r = cli.base_roots();
        assert_eq!(
            r.eprefix().unwrap().as_str(),
            "/opt/p",
            "--prefix must set EPREFIX (relocatable installed tree)"
        );
        // Overlay: base is the host (None), not the prefix.
        assert_eq!(r.base(), None, "--prefix base is the host (overlay)");
    }

    /// `--prefix` BROOT is the host: `base_roots().merge_root()` (BROOT, where
    /// preflight checks BDEPEND) is `/`, while `roots().merge_root()` (the
    /// actual install target) is the prefix. These two genuinely differ for an
    /// overlay; conflating them made preflight check jinja2's BDEPEND against
    /// the empty prefix VDB instead of the host, failing the build.
    /// See docs/root-topology.md § "Override semantics".
    #[test]
    fn prefix_overlay_broot_is_host_not_prefix() {
        let cli = Cli::parse_from(["em", "--prefix", "/opt/p", "-p", "sys-libs/zlib"]);
        // BROOT (base_roots) → host `/`.
        assert_eq!(
            cli.base_roots().merge_root().as_str(),
            "/",
            "base_roots().merge_root() must be the host (BROOT) under --prefix"
        );
        // Install target (roots) → the prefix.
        assert_eq!(
            cli.roots().merge_root().as_str(),
            "/opt/p",
            "roots().merge_root() must be the prefix (install target) under --prefix"
        );
    }
}

#[derive(Subcommand)]
pub enum Applet {
    /// Run one do*/new* install helper standalone against the exported build
    /// env. Internal: backs the PATH shims dropped during a build so
    /// `find -exec doman` / `xargs do*` reach helpers that are in-shell
    /// builtins. Not for direct use.
    #[command(name = "__helper", hide = true)]
    Helper {
        /// Helper name (e.g. `doman`, `dolib.a`).
        name: String,
        /// Arguments passed through to the helper.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Internal: the privilege-wrapped install worker (install+qmerge+binpkg
    /// for one package; spawned per package by `build_and_merge`).
    #[command(name = "__worker", hide = true)]
    Worker {
        #[arg(long)]
        ebuild: String,
        #[arg(long)]
        use_flags: String,
        #[arg(long)]
        work_base: String,
        #[arg(long)]
        root: String,
        #[arg(long)]
        distdir: Option<String>,
        #[arg(long)]
        config_root: Option<String>,
        #[arg(long)]
        sysroot: Option<String>,
        #[arg(long)]
        eprefix: Option<String>,
        /// A pre-built GPKG to merge (`-k`/`-g`).
        #[arg(long)]
        binpkg: Option<String>,
        #[arg(long)]
        buildpkg: bool,
        #[arg(long)]
        quiet: bool,
    },

    #[command(about = "Execute ebuild phases")]
    Ebuild {
        #[arg(required = true)]
        ebuild_path: String,
        #[arg(required = true)]
        phase: Vec<String>,
        /// Override the build work directory (default: `/var/tmp/portage/<cat>/<pf>`)
        #[arg(short = 'w', long, value_name = "DIR")]
        work_dir: Option<camino::Utf8PathBuf>,
    },

    #[command(about = "System maintenance and health checks")]
    Maint {
        #[command(subcommand)]
        command: Option<MaintCommand>,
    },

    #[command(about = "Query Portage internal variables and data")]
    Portageq {
        #[arg(required = true)]
        command: String,
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    #[command(about = "Sync repositories")]
    Sync { repos: Vec<String> },

    #[command(about = "Remove orphaned/unused packages")]
    Depclean {
        #[arg(trailing_var_arg = true)]
        atoms: Vec<String>,
    },

    #[command(about = "Regenerate metadata cache")]
    Regen {
        repos: Vec<String>,
        /// Write cache files to this directory instead of metadata/md5-cache
        #[arg(short = 'o', long, value_name = "DIR")]
        output: Option<std::path::PathBuf>,
        /// Directory containing master repositories
        #[arg(long, value_name = "DIR")]
        repos_dir: Option<String>,
        /// Number of parallel workers
        #[arg(short = 'j', long)]
        jobs: Option<usize>,
        /// Deduplicate top-level dep tokens before writing
        #[arg(long)]
        dedup: bool,
    },

    #[command(about = "Create binary packages from installed files")]
    Quickpkg {
        #[arg(required = true)]
        atoms: Vec<String>,
    },

    #[command(about = "Fetch/mirror distfiles")]
    Mirror {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    #[command(about = "Query package information")]
    Query {
        #[command(subcommand)]
        command: QueryCommand,
    },

    #[command(about = "Clean distfiles and/or binary packages")]
    Clean {
        #[command(subcommand)]
        target: Option<CleanTarget>,
    },

    #[command(about = "Enable/disable/query USE flags in make.conf")]
    Use {
        /// Add (enable) flags
        #[arg(short = 'a', long = "add", value_name = "FLAG")]
        add: Vec<String>,
        /// Remove (disable) flags
        #[arg(short = 'r', long = "remove", value_name = "FLAG")]
        remove: Vec<String>,
        /// Path to make.conf (default: /etc/portage/make.conf)
        #[arg(long = "make-conf", value_name = "PATH")]
        make_conf: Option<camino::Utf8PathBuf>,
    },

    #[command(about = "Edit per-package configuration (package.use, .keywords, .mask, .env)")]
    Pkg {
        #[command(subcommand)]
        command: PkgCommand,
    },

    #[command(about = "Rebuild packages with broken shared library deps")]
    Revdep {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    #[command(about = "Display Portage elog files")]
    Read {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    #[command(about = "Read/manage GLEP 42 news items")]
    News {
        #[command(subcommand)]
        command: Option<NewsCommand>,
    },

    #[command(about = "Check Gentoo Linux Security Advisories")]
    Glsa {
        #[command(subcommand)]
        command: Option<GlsaCommand>,
    },

    #[command(about = "Analyze emerge.log")]
    Log {
        #[command(subcommand)]
        command: Option<LogCommand>,
    },

    #[command(about = "Search inside ebuilds and eclasses")]
    Grep {
        #[arg(required = true)]
        pattern: String,
        #[arg(trailing_var_arg = true)]
        paths: Vec<String>,
    },

    #[command(about = "Search package names and descriptions")]
    Search {
        /// List all packages (no pattern required)
        #[arg(short = 'a', long)]
        all: bool,
        /// Search package descriptions instead of names
        #[arg(short = 'S', long = "desc")]
        desc: bool,
        /// Show only package name, no description
        #[arg(short = 'N', long = "name-only")]
        name_only: bool,
        /// Show homepage instead of description
        #[arg(short = 'H', long)]
        homepage: bool,
        /// Pattern to search (required unless --all)
        #[arg(required_unless_present = "all")]
        pattern: Option<String>,
    },

    #[command(about = "Parse/split atom strings")]
    Atom {
        #[arg(required = true)]
        atoms: Vec<String>,
    },

    #[command(about = "Native config selectors (profile, repos) — eselect-like")]
    Select {
        #[command(subcommand)]
        command: SelectCommand,
    },

    #[command(about = "Bootstrap a prefix layout (use with --local or --prefix)")]
    Setup,

    #[command(about = "Set up a cross-compilation target (sysroot + overlay) — crossdev workalike")]
    Crossdev(CrossdevArgs),

    #[command(
        about = "Bootstrap a self-hosting native toolchain into --root (the stages' compiler)"
    )]
    Toolchain(ToolchainArgs),

    #[command(about = "Assemble stage-build artifacts (stage1 packages.build) into --root")]
    Stages(StagesArgs),

    #[command(about = "Safe configuration file updates (dispatch-conf)")]
    Dispatch,

    #[command(about = "Interactive configuration file updates (etc-update)")]
    Etc,

    #[command(about = "Regenerate /etc/profile.env and ld.so cache")]
    Env,
}

/// `em crossdev` — cross-target setup, mirroring crossdev's option surface (the
/// no-build subset for now; building the toolchain is future work).
#[derive(clap::Args)]
pub struct CrossdevArgs {
    /// Target tuple ARCH-VENDOR-OS-LIBC (e.g. `riscv64-unknown-linux-gnu`,
    /// `aarch64-unknown-linux-musl`, `riscv64-unknown-elf`).
    #[arg(short = 't', long = "target", value_name = "TUPLE")]
    pub target: String,

    /// Use the LLVM/Clang model (`cross_llvm-*`: host clang cross-targets, no
    /// per-target compiler). Rejects glibc — use musl or a bare-metal target.
    #[arg(short = 'L', long)]
    pub llvm: bool,

    /// Lay down the overlay + sysroot config without building anything.
    #[arg(long)]
    pub init_target: bool,

    /// Bootstrap the cross toolchain into the prefix (`/usr/<tuple>`): the full
    /// intertwined sequence (binutils → headers → gcc-stage1 → libc →
    /// gcc-stage2). Implies `--init-target`.
    #[arg(long)]
    pub setup: bool,

    /// Print the derived target configuration and exit (no writes).
    #[arg(long)]
    pub show_target_cfg: bool,

    #[command(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[command(flatten)]
    pub merge_flags: MergeFlags,
}

/// `em toolchain` — bootstrap a self-hosting native toolchain into `--root`.
///
/// The native twin of `crossdev --setup` (`CHOST == CBUILD`): the staged
/// `baselayout → binutils → os-headers → glibc → gcc` bootstrap that produces a
/// working compiler + libc in a fresh ROOT. This is the *toolchain* primitive —
/// the compiler the `em stages` production (stage1 `packages.build`, stage3
/// `--emptytree @system`) then builds against. Kept separate from the stages on
/// purpose (catalyst/crossdev-stages do the same: toolchain, then the stages).
#[derive(clap::Args, Debug, Clone)]
pub struct ToolchainArgs {
    /// Build and install the toolchain into `--root` (the only action for now;
    /// required, mirroring `crossdev --setup`).
    #[arg(long)]
    pub setup: bool,

    #[command(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[command(flatten)]
    pub merge_flags: MergeFlags,
}

/// `em stages` — assemble stage-build artifacts (stage1/stage3/stage4) *using*
/// a toolchain already built by `em toolchain --setup`. See
/// `todo/em-stages-and-binhosts.md`.
#[derive(clap::Args, Debug, Clone)]
pub struct StagesArgs {
    /// Emerge the profile's `packages.build` bootstrap set into `--root`:
    /// baselayout (USE=build, --nodeps) then the minimal stage1 package list
    /// (USE="-* build"), mirroring catalyst's `stage1/chroot.sh`. Requires a
    /// working toolchain already in the root (`em toolchain --setup`).
    #[arg(long)]
    pub stage1: bool,

    #[command(flatten)]
    pub depgraph_flags: DepgraphFlags,

    #[command(flatten)]
    pub merge_flags: MergeFlags,
}

#[derive(Subcommand)]
pub enum MaintCommand {
    #[command(about = "Run all maintenance tasks")]
    All,
    #[command(about = "Generate binary package metadata index")]
    Binhost,
    #[command(about = "Discard stale config tracker entries")]
    Cleanconfmem,
    #[command(about = "Discard saved resume lists")]
    Cleanresume,
    #[command(about = "Clean old Portage build logs")]
    Logs,
    #[command(about = "Scan for and fix failed merges")]
    Merges,
    #[command(about = "Apply package moves to binary packages")]
    Movebin,
    #[command(about = "Apply package moves to installed packages")]
    Moveinst,
    #[command(about = "Regenerate profiles/use.local.desc from metadata.xml")]
    RegenUse {
        /// Write output here instead of profiles/use.local.desc ('-' for stdout)
        #[arg(short, long, value_name = "PATH")]
        output: Option<String>,
    },
    #[command(about = "Purge repo revision history from repo_revisions")]
    Revisions {
        /// Purge only these repos (default: all)
        #[arg(value_name = "REPO")]
        repos: Vec<String>,
    },
    #[command(about = "Sync repositories")]
    Sync { repos: Vec<String> },
    #[command(about = "Check (and optionally fix) problems in the world file")]
    World {
        /// Remove orphaned entries from the world file
        #[arg(short, long)]
        fix: bool,
    },
}

/// `em select <module>` — native, eselect-like config selectors.
#[derive(Subcommand)]
pub enum SelectCommand {
    #[command(about = "Select the system/sysroot profile (cross-aware)")]
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
    #[command(
        visible_alias = "repos",
        about = "Manage local repositories (overlays)"
    )]
    Repository {
        #[command(subcommand)]
        action: RepositoryAction,
    },
    #[command(
        visible_alias = "gcc",
        about = "Select the active compiler profile (gcc-config/eselect gcc workalike)"
    )]
    Compiler {
        #[command(subcommand)]
        action: CompilerAction,
    },
    #[command(
        about = "Select the active binutils profile (binutils-config/eselect binutils workalike)"
    )]
    Binutils {
        #[command(subcommand)]
        action: BinutilsAction,
    },
    #[command(about = "Select the active linker profile")]
    Linker {
        #[command(subcommand)]
        action: LinkerAction,
    },
    #[command(about = "Select the active LLVM/clang slot")]
    Clang {
        #[command(subcommand)]
        action: ClangAction,
    },
    #[command(
        visible_alias = "mirror",
        about = "Manage Gentoo distfile mirrors (mirrorselect workalike)"
    )]
    Mirrors {
        #[command(subcommand)]
        action: MirrorAction,
    },
}

/// `em select profile <action>`.
#[derive(Subcommand)]
pub enum ProfileAction {
    #[command(about = "List available profiles (marks the current one)")]
    List,
    #[command(about = "Show the current profile")]
    Show,
    #[command(about = "Set the profile by list number or path (cross-aware: no arch check)")]
    Set {
        /// Profile list number (from `list`) or path (e.g. `default/linux/riscv/23.0/rv64/lp64d`).
        target: String,
    },
}

/// `em select repository <action>` — local repos only (remote sync is a TODO).
#[derive(Subcommand)]
pub enum RepositoryAction {
    #[command(about = "List configured repositories")]
    List,
    #[command(about = "Register an existing local repository")]
    Add {
        /// Repository name.
        name: String,
        /// Existing local path to the repository.
        location: String,
    },
    #[command(visible_alias = "rm", about = "Remove a repository's repos.conf entry")]
    Remove {
        /// Repository name.
        name: String,
    },
    #[command(about = "Create a new local overlay (skeleton + repos.conf entry)")]
    Create {
        /// Repository name.
        name: String,
        /// Location (default: `<config-root>/var/db/repos/<name>`).
        location: Option<String>,
    },
}

/// `em select compiler <action>` — gcc-config workalike.
#[derive(Subcommand)]
pub enum CompilerAction {
    #[command(about = "List available compiler profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Show the current compiler profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Set the active compiler profile")]
    Set {
        /// Compiler profile to activate (e.g., `riscv64-unknown-linux-gnu-16` or `1` for list number).
        profile: String,
        /// Target tuple (CTARGET) for cross-compiler selection.
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select binutils <action>` — binutils-config workalike.
#[derive(Subcommand)]
pub enum BinutilsAction {
    #[command(about = "List available binutils profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Show the current binutils profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Set the active binutils profile")]
    Set {
        /// Binutils profile to activate (e.g., `riscv64-unknown-linux-gnu-2.46.0` or `1` for list number).
        profile: String,
        /// Target tuple (CTARGET) for cross-binutils selection.
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select linker <action>` — linker profile selection.
#[derive(Subcommand)]
pub enum LinkerAction {
    #[command(about = "List available linker profiles")]
    List {
        /// Target tuple (CTARGET) to list profiles for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Show the current linker profile")]
    Show {
        /// Target tuple (CTARGET) to show profile for.
        #[arg(short, long)]
        target: Option<String>,
    },
    #[command(about = "Set the active linker profile")]
    Set {
        /// Linker profile to activate (e.g., `riscv64-unknown-linux-gnu-lld-18` or `1` for list number).
        profile: String,
        /// Target tuple (CTARGET) for cross-linker selection.
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// `em select clang <action>` — LLVM/clang slot selection.
#[derive(Subcommand)]
pub enum ClangAction {
    #[command(about = "List available LLVM/clang slots")]
    List,
    #[command(about = "Show the current LLVM/clang slot")]
    Show,
    #[command(about = "Set the active LLVM/clang slot")]
    Set {
        /// LLVM slot to activate (e.g., `22` or `1` for list number).
        slot: String,
    },
}

/// `em select mirrors <action>` — mirrorselect workalike for `GENTOO_MIRRORS`.
#[derive(Subcommand)]
pub enum MirrorAction {
    /// List available Gentoo distfile mirrors (marks those already selected).
    List {
        /// Keep only mirrors in this ISO country code (e.g. `US`, `DE`).
        #[arg(short, long)]
        country: Option<String>,
        /// Keep only mirrors in this region (e.g. `Europe`, `North America`).
        #[arg(short, long)]
        region: Option<String>,
    },
    /// Show the currently configured `GENTOO_MIRRORS` value.
    Show,
    /// Set `GENTOO_MIRRORS`.
    Set {
        /// Explicit mirror URLs to use. If omitted, mirrors are picked from
        /// `--country`/`--region` instead.
        #[arg(value_name = "URL")]
        urls: Vec<String>,
        /// Use every mirror in this ISO country code.
        #[arg(short, long)]
        country: Option<String>,
        /// Use every mirror in this region.
        #[arg(short, long)]
        region: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum PkgCommand {
    #[command(about = "Edit per-package USE flags in package.use")]
    Use {
        /// Package atom (e.g. sys-boot/grub or >=dev-libs/foo-1.0)
        atom: String,
        /// Add flags (written verbatim, e.g. truetype)
        #[arg(short = 'a', long = "add", value_name = "FLAG")]
        add: Vec<String>,
        /// Subtract flags (written with leading '-', e.g. -themes)
        #[arg(short = 's', long = "subtract", value_name = "FLAG")]
        subtract: Vec<String>,
        /// Drop flags entirely (removes both flag and -flag forms)
        #[arg(short = 'd', long = "drop", value_name = "FLAG")]
        drop: Vec<String>,
        /// Target file inside package.use/ (default: `<cat>-<pkg>`)
        #[arg(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[command(about = "Edit per-package keywords in package.accept_keywords")]
    Keyword {
        atom: String,
        #[arg(short = 'a', long = "add", value_name = "KW")]
        add: Vec<String>,
        #[arg(short = 's', long = "subtract", value_name = "KW")]
        subtract: Vec<String>,
        #[arg(short = 'd', long = "drop", value_name = "KW")]
        drop: Vec<String>,
        #[arg(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[command(about = "Add/remove a package from package.mask")]
    Mask {
        atom: String,
        /// Add the atom to package.mask
        #[arg(short = 'a', long = "add")]
        add: bool,
        /// Remove the atom from package.mask
        #[arg(short = 'd', long = "drop")]
        drop: bool,
        #[arg(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
    #[command(about = "Edit per-package env files in package.env")]
    Env {
        atom: String,
        #[arg(short = 'a', long = "add", value_name = "ENVFILE")]
        add: Vec<String>,
        #[arg(short = 'd', long = "drop", value_name = "ENVFILE")]
        drop: Vec<String>,
        #[arg(long, value_name = "FILE")]
        path: Option<camino::Utf8PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum QueryCommand {
    #[command(about = "Find which package owns a file", alias = "b")]
    Belongs {
        #[arg(required = true)]
        file: Vec<String>,
    },
    #[command(about = "Verify checksums of installed package", alias = "k")]
    Check {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "List packages depending on an atom", alias = "d")]
    Depends {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "Display full dependency tree", alias = "g")]
    Depgraph {
        #[arg(required = true)]
        atom: Vec<String>,
        /// Output format
        #[arg(long, short, value_enum, default_value = "pretty")]
        format: DepgraphFormat,
        /// Let the solver choose USE flags to satisfy REQUIRED_USE (Level C).
        #[arg(long)]
        autosolve_use: bool,
        #[command(flatten)]
        depgraph_flags: DepgraphFlags,
        /// Treat every atom as not-yet-installed (emerge's `-e`/`--emptytree`).
        #[arg(short = 'e', long)]
        emptytree: bool,
        #[arg(short = 'o', long)]
        onlydeps: bool,
        /// Include build-time dependencies (BDEPEND) in the resolution.
        #[arg(long)]
        with_bdeps: bool,
        /// emerge's `--root-deps[=rdeps]`: only require RDEPEND (not DEPEND)
        /// to be satisfiable in the merge target.
        #[arg(long = "root-deps")]
        root_deps: bool,
    },
    #[command(about = "List files installed by a package", alias = "f")]
    Files {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "List packages matching env data", alias = "a")]
    Has {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "List packages with a given USE flag in IUSE", alias = "h")]
    Hasuse {
        #[arg(required = true)]
        flag: Vec<String>,
    },
    #[command(about = "Display keyword status across architectures", alias = "y")]
    Keywords {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "List installed/available packages matching a pattern")]
    List {
        /// List only installed packages (from VDB), not available ones
        #[arg(short = 'I', long = "installed")]
        installed: bool,
        /// Glob or substring pattern(s); omit to list all packages
        #[arg()]
        pattern: Vec<String>,
    },
    #[command(
        about = "Display package metadata (maintainer, homepage, etc.)",
        alias = "m"
    )]
    Meta {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "Display total file size of a package", alias = "s")]
    Size {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "Display USE flags for a package", alias = "u")]
    Uses {
        #[arg(required = true)]
        atom: Vec<String>,
    },
    #[command(about = "Print full path to the ebuild for a package", alias = "w")]
    Which {
        #[arg(required = true)]
        atom: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum CleanTarget {
    #[command(about = "Clean outdated distfiles")]
    Dist,
    #[command(about = "Clean outdated binary packages")]
    Pkg,
}

#[derive(Subcommand)]
pub enum NewsCommand {
    #[command(about = "Count unread news items")]
    Count,
    #[command(about = "List news items")]
    List,
    #[command(about = "Read a news item")]
    Read { id: Option<String> },
    #[command(about = "Purge read news items")]
    Purge,
}

#[derive(Subcommand)]
pub enum GlsaCommand {
    #[command(about = "List all GLSAs")]
    List,
    #[command(about = "Check for affected GLSAs")]
    Check { ids: Vec<String> },
    #[command(about = "Apply a GLSA fix")]
    Fix { ids: Vec<String> },
}

#[derive(Subcommand)]
pub enum LogCommand {
    #[command(about = "Show currently running merges")]
    Current,
    #[command(about = "Show merge history")]
    List { limit: Option<u32> },
    #[command(about = "Show merge times for a package")]
    Time { atom: Option<String> },
}

/// How an unprivileged build gets root for `chown`/setuid (see `--privilege`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Privilege {
    /// Best compiled-in fake root (pseudoroot, else fakeroost, else none) when
    /// unprivileged, real chowns when already root (default).
    #[default]
    Auto,
    /// Pure-Rust ptrace+seccomp fake root; ownership faked in-session.
    #[cfg(all(feature = "fakeroost", target_os = "linux"))]
    Fakeroost,
    /// LD_PRELOAD fake root (`pseudoroot`); ownership faked in-session, no ptrace tax.
    #[cfg(all(feature = "pseudoroot", any(target_os = "linux", target_os = "macos")))]
    Pseudoroot,
    /// User-namespace sandbox with build-user→0 map; real chowns in-box.
    #[cfg(all(feature = "hakoniwa", target_os = "linux"))]
    Hakoniwa,
    /// Re-exec under `sudo` for real root (root-owned tree, real setuid).
    Sudo,
    /// No wrapping; run unprivileged (chowns best-effort, may not stick).
    None,
}

/// Output format for `em query depgraph`.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum DepgraphFormat {
    /// emerge -p style pretend output
    Pretty,
    /// Machine-parsable JSON
    Json,
    /// cargo tree style dependency tree
    Tree,
}

fn parse_arch(s: &str) -> std::result::Result<Arch, String> {
    Arch::from_str(s).map_err(|e| e.to_string())
}
