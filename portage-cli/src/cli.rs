use std::str::FromStr;

use clap::builder::styling::{AnsiColor, Styles};
use clap::{Parser, Subcommand};
use gentoo_core::Arch;

const fn cli_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Yellow.on_default().bold())
        .usage(AnsiColor::Green.on_default().bold())
        .literal(AnsiColor::Green.on_default())
        .placeholder(AnsiColor::Cyan.on_default())
        .error(AnsiColor::Red.on_default().bold())
        .valid(AnsiColor::Green.on_default())
        .invalid(AnsiColor::Red.on_default())
}

#[derive(Parser)]
#[command(
    name = "em",
    version,
    about = "Gentoo Portage package manager CLI",
    arg_required_else_help = true,
    styles = cli_styles()
)]
pub struct Cli {
    #[command(flatten)]
    pub color: colorchoice_clap::Color,

    #[arg(short = 'p', long)]
    pub pretend: bool,

    #[arg(short = 'a', long)]
    pub ask: bool,

    #[arg(short = 'v', long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    #[arg(short = 'D', long)]
    pub deep: bool,

    #[arg(long, value_name = "ARCH", default_value_t = Arch::current(), value_parser = parse_arch)]
    pub arch: Arch,

    /// Pin search/query to a single repository. When unset, repositories are
    /// auto-discovered from `repos.conf` (the main repo wins for single-repo
    /// applets; `em search` walks all of them).
    #[arg(long, value_name = "PATH")]
    pub repo: Option<String>,

    /// Unprivileged offset: merge ROOT, the VDB, distfiles and build trees
    /// all live under DIR. Configuration (profile, make.conf) still comes
    /// from the host; use --root for a full config offset.
    #[arg(long, value_name = "DIR", global = true)]
    pub prefix: Option<String>,

    /// Bootstrap the prefix layout before building: skeleton directories, a
    /// `bashrc` overlay search-path recipe, and a commented `make.conf`. Use
    /// with `--local` (`~/.gentoo`) or `--prefix DIR`. Safe to re-run (never
    /// clobbers existing files). With no atoms it just sets up and exits.
    #[arg(long, global = true)]
    pub setup: bool,

    /// Unprivileged in-place install into a Gentoo-Prefix at `~/.gentoo`
    /// (`EPREFIX=~/.gentoo`): packages are configured for and installed to
    /// `~/.gentoo/usr/...`, usable in place (add `~/.gentoo/usr/bin` to PATH).
    /// Profile/make.conf come from the host; `~/.gentoo/etc/portage` overlays
    /// `package.use`/`bashrc` (see `--setup`). Full XDG layout is issue #2.
    #[arg(long, global = true)]
    pub local: bool,

    /// Search package names, like emerge --search (each argument is a pattern)
    #[arg(short = 's', long)]
    pub search: bool,

    /// Search package names and descriptions, like emerge --searchdesc
    #[arg(short = 'S', long)]
    pub searchdesc: bool,

    #[arg(short = 'N', long)]
    pub newuse: bool,

    #[arg(short = 'u', long)]
    pub update: bool,

    /// Write required USE changes to /etc/portage/package.use/
    #[arg(long)]
    pub autounmask_write: bool,

    #[arg(short = '1', long = "oneshot")]
    pub oneshot: bool,

    #[arg(short = 'f', long)]
    pub fetchonly: bool,

    #[arg(short = 'b', long)]
    pub buildpkg: bool,

    #[arg(short = 'k', long)]
    pub usepkg: bool,

    #[arg(short = 'K', long)]
    pub usepkgonly: bool,

    #[arg(short = 'g', long)]
    pub getbinpkg: bool,

    #[arg(short = 'G', long)]
    pub getbinpkgonly: bool,

    #[arg(short = 'e', long)]
    pub emptytree: bool,

    #[arg(short = 't', long)]
    pub tree: bool,

    #[arg(short = 'O', long)]
    pub nodeps: bool,

    #[arg(short = 'o', long)]
    pub onlydeps: bool,

    #[arg(short = 'n', long)]
    pub noreplace: bool,

    /// Build up to N packages in parallel, respecting build-dependency order
    /// (merges are still serialised). Default 1 (sequential).
    #[arg(short = 'j', long, value_name = "N")]
    pub jobs: Option<u32>,

    #[arg(short = 'l', long, value_name = "LOAD")]
    pub load_average: Option<f64>,

    #[arg(long)]
    pub keep_going: bool,

    #[arg(long)]
    pub autounmask: bool,

    /// Let the solver choose USE flags to satisfy REQUIRED_USE (Level C) rather
    /// than only reporting violations. Off by default; flips are reported.
    #[arg(long)]
    pub autosolve_use: bool,

    #[arg(long)]
    pub complete_graph: bool,

    #[arg(long)]
    pub with_bdeps: bool,

    #[arg(short = 'X', long, value_name = "ATOM")]
    pub exclude: Vec<String>,

    /// Installation root (the offset all applets install into / query).
    #[arg(long, env = "ROOT", value_name = "PATH", global = true)]
    pub root: Option<String>,

    /// Read config (profile, make.conf) from this root instead of `--root`.
    #[arg(long, value_name = "PATH", global = true)]
    pub config_root: Option<String>,

    /// Override VDB path (default: $ROOT/var/db/pkg)
    #[arg(long, value_name = "PATH", global = true)]
    pub vdb: Option<String>,

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
    /// Resolve the root model (docs/root-model.md) from the global flags.
    pub fn roots(&self) -> Roots {
        let path = |s: &Option<String>| s.as_deref().map(camino::Utf8PathBuf::from);
        // `--local`: in-place Gentoo-Prefix at ~/.gentoo. Profile/make.conf come
        // from the host; ~/.gentoo/etc/portage overlays package.use/bashrc.
        // EPREFIX == target == ~/.gentoo, so EROOT == target and ROOT == /.
        if self.local {
            let prefix = home_dir().join(".gentoo");
            return Roots {
                config: None,
                base: None,
                target: Some(prefix.clone()),
                eprefix: Some(prefix.clone()),
                config_overlay: Some(prefix.join("etc/portage")),
                relocate: true,
            };
        }
        Roots {
            // config: --config-root, else --root; host otherwise.
            config: path(&self.config_root).or_else(|| path(&self.root)),
            // base: --root; host otherwise. --prefix never changes it.
            base: path(&self.root),
            // target: --prefix (install destination), else --root.
            target: path(&self.prefix).or_else(|| path(&self.root)),
            eprefix: None,
            // A --prefix overlay reads prefix-local package.use/bashrc from
            // DIR/etc/portage (created by --setup); host config provides the
            // profile. None for host / --root (config is already offset).
            config_overlay: path(&self.prefix).map(|p| p.join("etc/portage")),
            // --prefix also relocates distfiles/build trees under the target.
            relocate: self.prefix.is_some(),
        }
    }

    /// Path used by single-repo applets. Falls back to `/var/db/repos/gentoo`
    /// when neither `--repo` nor `repos.conf` is available.
    pub fn repo_path(&self) -> String {
        if let Some(p) = &self.repo {
            return p.clone();
        }
        if let Ok(rc) = portage_repo::ReposConf::load()
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
        match portage_repo::ReposConf::load() {
            Ok(rc) if !rc.repos().is_empty() => {
                rc.repos().iter().map(|e| e.location.clone()).collect()
            }
            _ => vec![std::path::PathBuf::from("/var/db/repos/gentoo")],
        }
    }
}

#[derive(Subcommand)]
pub enum Applet {
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

    #[command(about = "Modular system configuration (eselect)")]
    Select {
        #[arg(required = true)]
        module: String,
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    #[command(about = "Safe configuration file updates (dispatch-conf)")]
    Dispatch,

    #[command(about = "Interactive configuration file updates (etc-update)")]
    Etc,

    #[command(about = "Regenerate /etc/profile.env and ld.so cache")]
    Env,
}

#[derive(Subcommand)]
pub enum MaintCommand {
    #[command(about = "Run all maintenance tasks")]
    All,
    #[command(about = "Generate binary package metadata index")]
    Binhost,
    #[command(about = "Discard stale config tracker entries")]
    Cleanconfmem,
    #[command(about = "Discard saved emerge --resume lists")]
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
