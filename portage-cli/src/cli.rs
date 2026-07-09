use std::str::FromStr;

use clap::builder::styling::{AnsiColor as ClapAnsiColor, Styles};
use clap::{Parser, Subcommand};
use gentoo_core::Arch;
use portage_atom_pubgrub::DepClass;

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

    /// Unprivileged, standalone Gentoo-Prefix: own VDB/BROOT/config, not
    /// overlaid on the host (see --prefix for the overlay). Defaults to
    /// ~/.gentoo (EPREFIX=~/.gentoo) when no DIR is given.
    #[arg(long, global = true, num_args = 0..=1, default_missing_value = "", value_name = "DIR")]
    pub local: Option<String>,

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

    /// Cross-build/setup for a crossdev target tuple. The single source for
    /// "which tuple" everywhere: `em --target T crossdev --init-target`
    /// sets T up; `em --target T stages --stage1` (or any plain atom build)
    /// resolves/installs into the target sysroot `<EROOT>/usr/<TUPLE>` (the
    /// crossdev `<TUPLE>-emerge` entry point) — sugar for `--config-root
    /// <sysroot> --root <sysroot>`, with the cross context (CHOST/CBUILD,
    /// `--root-deps=rdeps`) read from the sysroot make.conf. One flag for
    /// both roles, not two that can disagree — `crossdev` no longer has its
    /// own `-t`/`--target`.
    #[arg(long, short = 'T', value_name = "TUPLE", global = true)]
    pub target: Option<String>,

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
    /// Where `BDEPEND`/`IDEPEND` (cross) resolve — always the true build
    /// host, independent of any `--target` sysroot substitution. `None`
    /// only where it trivially equals `merge_root()` (bare, `--local`).
    /// See [`satisfaction_root`](Self::satisfaction_root).
    broot: Option<camino::Utf8PathBuf>,
    /// `CHOST != CBUILD` for the currently active topology — the one cell
    /// `satisfaction_root` needs it for (`IDEPEND`).
    is_cross_arch: bool,
    /// `EPREFIX`: when set (`--local`), packages are configured for and
    /// installed in place at this offset (`target == eprefix`, so `EROOT ==
    /// target` and `ROOT == /`). `None` for ROOT-offset / host builds.
    eprefix: Option<camino::Utf8PathBuf>,
    /// A user-writable config dir overlaid on the host config for
    /// `package.use`/`bashrc` (the `~/.gentoo/etc/portage` of `--local`),
    /// so an unprivileged user can override without touching `/etc/portage`.
    config_overlay: Option<camino::Utf8PathBuf>,
    relocate: bool,
    /// The literal `--config-root` value, if the user gave one — unlike
    /// [`config`](Self::config), never derived from `--root`. See
    /// [`config_root_explicit`](Self::config_root_explicit).
    config_root_explicit: Option<camino::Utf8PathBuf>,
}

impl Roots {
    /// `PORTAGE_CONFIGROOT`: where profile and make.conf are read.
    pub fn config(&self) -> Option<&camino::Utf8Path> {
        self.config.as_deref()
    }

    /// The literal `--config-root` value, if given — unlike
    /// [`config`](Self::config), never derived from `--root`. `em select`
    /// uses this instead of `config()`, matching real eselect's own
    /// behavior (its `profile.eselect` module only ever honours an explicit
    /// `PORTAGE_CONFIGROOT`/`EROOT`, never derives a config root from `ROOT`
    /// alone) — so a bare `em --root R select ...` operates on the host's
    /// config unless `--config-root R` is also given, instead of silently
    /// picking up whatever `--root`'s self-contained-bootstrap default
    /// resolved `config()` to.
    pub(crate) fn config_root_explicit(&self) -> Option<&camino::Utf8Path> {
        self.config_root_explicit.as_deref()
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

    /// Whether this is a self-contained `--root DIR` topology (own config,
    /// own everything — `setup.rs`'s "self-contained offset" mode): no
    /// EPREFIX, base == target, and not the bare host. Topology-only — a
    /// robust replacement for the old `config().is_some()` proxy
    /// (`config()` incidentally happens to be `Some` for exactly this
    /// topology too, but that's no longer the *reason* to detect it — see
    /// `config_root_explicit`). Used by `crossdev/mod.rs`'s
    /// `ensure_self_contained_prefix`/`ensure_prefix_profile`.
    pub(crate) fn is_self_contained_root(&self) -> bool {
        self.eprefix.is_none() && self.base == self.target && self.merge_root().as_str() != "/"
    }

    /// For internal orchestration only (`crossdev::activate_toolchain`):
    /// a self-contained `--root` build's own `gcc-config`/`binutils-config`
    /// slot files must live under *its own* `etc/env.d`, not the host's —
    /// unlike `em select`'s user-facing config-root resolution
    /// (`config_root_explicit`), which deliberately does NOT infer this from
    /// `--root` alone (see that method's doc comment). The internal
    /// orchestrator already knows it just bootstrapped this exact offset, so
    /// it forces its own config root explicitly rather than requiring the
    /// user to also type `--config-root` on every crossdev invocation.
    pub(crate) fn with_own_config_root_if_self_contained(mut self) -> Self {
        if self.is_self_contained_root() {
            self.config_root_explicit = Some(self.merge_root().to_owned());
        }
        self
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

    /// Where an unsatisfied dependency of `class` resolves and is checked
    /// against (docs/root-topology.md's satisfaction-root table, PMS table
    /// 8.2): `BDEPEND` always resolves on `broot` (the true build host,
    /// independent of any `--target` sysroot substitution); `IDEPEND` is
    /// `broot` for a cross build, else the same as `RDEPEND`/`PDEPEND`;
    /// `DEPEND` resolves against `base` when it genuinely differs from the
    /// target (an overlay, e.g. `--prefix`) else the target itself;
    /// `RDEPEND`/`PDEPEND` always resolve against the target (`merge_root()`).
    ///
    /// This replaces threading a second `host_roots: &Roots` alongside
    /// `roots` everywhere just to answer the `BDEPEND` question — `broot`
    /// is carried on the same `Roots` value now, so one value answers both.
    pub(crate) fn satisfaction_root(&self, class: DepClass) -> &camino::Utf8Path {
        match class {
            DepClass::Bdepend => self.broot.as_deref().unwrap_or_else(|| self.merge_root()),
            DepClass::Idepend if self.is_cross_arch => self.satisfaction_root(DepClass::Bdepend),
            DepClass::Idepend | DepClass::Rdepend | DepClass::Pdepend => self.merge_root(),
            DepClass::Depend => {
                if self.base.as_deref().is_some_and(|b| b != self.merge_root()) {
                    self.base.as_deref().unwrap()
                } else {
                    self.merge_root()
                }
            }
        }
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

    /// Test-only: a `Roots` with `base`, `target`, and `broot` all set to
    /// the same path (matching a plain `--root DIR` invocation, BROOT
    /// included, so BDEPEND-satisfaction tests see the same root without a
    /// separate `host_roots` value), for exercising root-selection logic
    /// without a full CLI parse and without any VDB lookup silently falling
    /// through to the real bare host's.
    #[cfg(test)]
    pub(crate) fn for_test(target: &str) -> Self {
        let path = camino::Utf8PathBuf::from(target);
        Roots {
            base: Some(path.clone()),
            target: Some(path.clone()),
            broot: Some(path),
            ..Default::default()
        }
    }

    /// Test-only: a `Roots` shaped like `--prefix`'s overlay — `base: None`,
    /// `target`/`eprefix` the prefix, `broot` a separate host path — so
    /// `is_overlay()`/BDEPEND-weave tests can use two independent fake VDB
    /// dirs instead of the real host `/`.
    #[cfg(test)]
    pub(crate) fn for_test_overlay(host: &str, prefix: &str) -> Self {
        let prefix = camino::Utf8PathBuf::from(prefix);
        Roots {
            base: None,
            target: Some(prefix.clone()),
            broot: Some(camino::Utf8PathBuf::from(host)),
            eprefix: Some(prefix.clone()),
            config_overlay: Some(prefix.join("etc/portage")),
            relocate: true,
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

/// The four filesystem roles (docs/root-topology.md § "The four roles"),
/// collapsed by how many coincide. `Cli::base_roots()` (BROOT view) and
/// `Cli::roots()` (install-target view) both derive from the same
/// `Cli::root_set()`, so they can't drift independently — see
/// `todo/root-topology-refactor.md`.
enum RootSet {
    /// All four roles collapse to one path: the bare invocation, or
    /// `--local` (a standalone Gentoo-Prefix owns its own BROOT too).
    Single { root: camino::Utf8PathBuf },
    /// BROOT distinct from the install target. `--root R`: BROOT is always
    /// the real host `/` (portage `ROOT=`/`{target}-emerge` parity) — an
    /// offset install borrows the host's own BDEPEND tools; it does not
    /// need its own copy of them.
    #[allow(dead_code)] // target isn't read yet: base_roots()/roots() keep their
    // own, separate "outer EROOT" derivation (see broot()'s doc comment on
    // why that's a different question from BROOT). Kept here to match
    // docs/root-topology.md's proposed shape for the fuller migration this
    // enum is a first step of (todo/root-topology-refactor.md).
    Dual {
        broot: camino::Utf8PathBuf,
        target: camino::Utf8PathBuf,
    },
    /// BROOT, base (build-against sysroot), and target all distinct.
    /// `--prefix P`: broot = base = the host `/` (the overlay borrows host
    /// tools and builds against them), target = P.
    #[allow(dead_code)] // base/target aren't read yet, same reason as Dual above.
    Overlayed {
        broot: camino::Utf8PathBuf,
        base: camino::Utf8PathBuf,
        target: camino::Utf8PathBuf,
    },
}

impl RootSet {
    /// Where `BDEPEND` tools run and are checked against (BROOT).
    fn broot(&self) -> &camino::Utf8Path {
        match self {
            RootSet::Single { root } => root,
            RootSet::Dual { broot, .. } | RootSet::Overlayed { broot, .. } => broot,
        }
    }
}

/// `s.as_deref()` parsed as a path, or `None`.
fn opt_path(s: &Option<String>) -> Option<camino::Utf8PathBuf> {
    s.as_deref().map(camino::Utf8PathBuf::from)
}

impl Cli {
    /// The root model (docs/root-topology.md) from `--local`/`--prefix`/
    /// `--root`, before config/overlay concerns. `--local` > `--prefix` >
    /// `--root` > bare, matching `base_roots()`'s existing precedence.
    fn root_set(&self) -> RootSet {
        let host = camino::Utf8PathBuf::from("/");
        if let Some(local) = &self.local {
            let root = if local.is_empty() {
                home_dir().join(".gentoo")
            } else {
                camino::Utf8PathBuf::from(local)
            };
            return RootSet::Single { root };
        }
        if let Some(prefix) = opt_path(&self.prefix) {
            return RootSet::Overlayed {
                broot: host.clone(),
                base: host,
                target: prefix,
            };
        }
        if let Some(root) = opt_path(&self.root) {
            return RootSet::Dual {
                broot: host,
                target: root,
            };
        }
        RootSet::Single { root: host }
    }
}

impl Cli {
    /// Resolve the root model (docs/root-topology.md) from the global flags.
    ///
    /// `--target <tuple>` layers on top of the base model: it targets the crossdev
    /// sysroot `<EROOT>/usr/<tuple>` as both config-root and root (crossdev's
    /// `PORTAGE_CONFIGROOT == ROOT == SYSROOT`). The `<EROOT>` it sits under still
    /// comes from `--local`/`--prefix`/`--root`, so `em --local --target <t>`
    /// targets `~/.gentoo/usr/<t>`.
    ///
    /// Under `--prefix`, the returned `Roots`'s `merge_root()` is the **prefix**
    /// (install destination), while `base_roots()` returns a separate view whose
    /// `merge_root()` is the **host `/`** (BROOT, for BDEPEND checks). The two
    /// genuinely differ for an overlay; this split is what lets preflight check
    /// BDEPEND against the host while the merge lands in the prefix.
    pub fn roots(&self) -> Roots {
        // --target: layer the sysroot on top of the overlay target (the prefix),
        // not base_roots's BROOT (host /). Under --prefix the cross sysroot is
        // <prefix>/usr/<tuple>, and base_roots's merge_root is the host — so
        // derive the sysroot from the overlay's prefix (eprefix) when set.
        let Some(tuple) = self.target.as_deref() else {
            return self.outer_roots();
        };
        // The outer EROOT the sysroot sits under: the overlay prefix when set
        // (--prefix), else the offset (--root) or host / (bare) — never
        // `base_roots()`/`roots()` directly, which would double-apply this
        // same substitution if called recursively; `outer_roots()` is always
        // the pre-substitution view.
        let outer = self.outer_roots();
        let eroot = outer.merge_root().to_owned();
        let sysroot = eroot.join("usr").join(tuple);
        Roots {
            config: Some(sysroot.clone()),
            base: Some(sysroot.clone()),
            target: Some(sysroot),
            // BROOT never moves with `--target`: BDEPEND always resolves on
            // the true build host, carried over from the outer (pre-
            // substitution) view rather than left as the sysroot itself.
            broot: outer.broot.clone(),
            // `--target` is crossdev's cross-tuple flag; every real
            // invocation of it is a foreign-arch build (a same-arch use
            // would just be `--root`). No `IDepend` caller exists yet to
            // need finer CHOST/CBUILD-derived precision than this.
            is_cross_arch: true,
            eprefix: None,
            config_overlay: None,
            relocate: false,
            config_root_explicit: outer.config_root_explicit.clone(),
        }
    }

    /// The root view with any `--target` sysroot substitution undone: what
    /// [`roots`](Self::roots) returns when `--target` isn't set, computed
    /// **unconditionally** regardless of whether `self.target` happens to
    /// also be set. This is the "outer EROOT" — `--local`/`--prefix`'s
    /// prefix, `--root`'s offset, or host `/` — that every crossdev *setup*
    /// action (`crossdev/mod.rs`: `sysroot`, `setup_root`,
    /// `ensure_self_contained_prefix`, `ensure_prefix_profile`, `main_repo`,
    /// and `setup()`/`toolchain()`'s own top-level checks) must anchor to
    /// instead of `roots()`. Using `roots()` there was a real bug: if
    /// `--target T` happens to also be set on the same invocation as
    /// `crossdev -t T --init-target`, `roots()` is *already* the sysroot,
    /// so appending `usr/T` again doubly-nested it
    /// (`<EROOT>/usr/T/usr/T` instead of `<EROOT>/usr/T`) — reproduced live,
    /// see `todo/root-topology-refactor.md`.
    ///
    /// `stage1()`/`profile_stack()`/`resolve_gcc_version` deliberately keep
    /// using plain `roots()` — those genuinely want `--target`'s sysroot
    /// substitution (`em --target T stages --stage1` builds *into* the
    /// sysroot, by design).
    pub(crate) fn outer_roots(&self) -> Roots {
        let base = self.base_roots();
        if let Some(prefix) = base.eprefix.as_deref().filter(|_| base.is_overlay()) {
            return Roots {
                config: base.config.clone(),
                base: None,
                target: Some(prefix.to_path_buf()),
                broot: base.broot.clone(),
                is_cross_arch: base.is_cross_arch,
                eprefix: Some(prefix.to_path_buf()),
                config_overlay: Some(prefix.join("etc/portage")),
                relocate: true,
                config_root_explicit: base.config_root_explicit.clone(),
            };
        }
        base
    }

    /// The root model from `--local`/`--prefix`/`--root`/`--config-root`, before
    /// any `--target` sysroot override (see [`roots`](Self::roots)). Exposed at
    /// `pub(crate)` so the staged-build driver can install `cross-*` toolchain
    /// packages (which always live in the outer EROOT, never the sysroot
    /// subdirectory — see `crossdev/mod.rs`'s module doc) even from a
    /// `--target`-active invocation.
    ///
    /// `merge_root()` of the returned `Roots` is **the outer EROOT** (with
    /// `--target`'s sysroot substitution undone) — where `bypass_cross_root`
    /// toolchain-install steps land and where `write_cross_env`/
    /// `write_sysroot_config` (`crossdev/mod.rs`) write config. Under
    /// `--prefix` that's the host `/` (the overlay borrows host tools);
    /// under `--local`/`--root` it's the offset itself. **This is not
    /// necessarily BROOT** — for plain `--root` the two differ (BROOT is
    /// always the host, see [`broot`](Self::broot)); they only coincide for
    /// `--prefix`/`--local`, which is why this function used to be (mis)used
    /// for BDEPEND checks too. Use [`broot`](Self::broot) for that.
    pub(crate) fn base_roots(&self) -> Roots {
        let path = opt_path;
        // `--local`: standalone Gentoo-Prefix, own BROOT. Full closure (base
        // == target == the prefix), self-contained VDB. EPREFIX makes installed
        // scripts relocatable (shebangs reference ${EPREFIX}/usr/bin/...). The
        // prefix builds its own python via `toolchain --setup`; during bootstrap
        // the host compiler is reached via PATH, never via a symlink masquerading
        // as a prefix-owned file (that's the overlay's job — see --prefix below).
        // See docs/root-topology.md § "Override semantics".
        if self.local.is_some() {
            let RootSet::Single { root: prefix } = self.root_set() else {
                unreachable!("--local always resolves to RootSet::Single")
            };
            return Roots {
                config: None,
                base: Some(prefix.clone()),
                target: Some(prefix.clone()),
                broot: Some(prefix.clone()),
                is_cross_arch: false,
                eprefix: Some(prefix.clone()),
                config_overlay: Some(prefix.join("etc/portage")),
                relocate: true,
                config_root_explicit: path(&self.config_root),
            };
        }
        // `--prefix` overlay: BROOT is the host `/`. The prefix is the install
        // destination (target), but base_roots()'s merge_root() must be the host
        // because that's what preflight/bdepend_avail check BDEPEND against.
        // roots() reconstructs the prefix-target view on top of this.
        if let Some(prefix) = path(&self.prefix) {
            return Roots {
                config: path(&self.config_root),
                base: None,
                target: None, // BROOT = host `/`, NOT the prefix
                broot: Some(camino::Utf8PathBuf::from("/")),
                is_cross_arch: false,
                eprefix: Some(prefix.clone()),
                config_overlay: Some(prefix.join("etc/portage")),
                relocate: true,
                config_root_explicit: path(&self.config_root),
            };
        }
        Roots {
            // config: --config-root, else --root; host otherwise. This is
            // `em`'s own deliberate self-contained-bootstrap default (own
            // config, own everything — setup.rs's "self-contained offset"
            // mode) — NOT a portage `ROOT=` parity gap: `em select`'s config
            // resolution intentionally does NOT follow this fallback
            // (`Roots::config_root_explicit`, matching real eselect's actual
            // behavior — see its `profile.eselect` module, which only ever
            // honours an explicit `PORTAGE_CONFIGROOT`/`EROOT`, never derives
            // from `ROOT` alone), and `--config-root /` already gives literal
            // `ROOT=`-sharing parity for anything else that wants it.
            // Decided 2026-07-09 — see todo/root-topology-refactor.md.
            config: path(&self.config_root).or_else(|| path(&self.root)),
            // base: --root; host otherwise.
            base: path(&self.root),
            // target: --root (install destination). This is "the outer EROOT"
            // (bypass_cross_root, write_cross_env/write_sysroot_config in
            // crossdev/mod.rs all rely on this staying the offset for --root)
            // — a DIFFERENT thing from BROOT, see satisfaction_root's doc comment.
            target: path(&self.root),
            broot: Some(self.root_set().broot().to_owned()),
            is_cross_arch: false,
            eprefix: None,
            config_overlay: None,
            relocate: false,
            config_root_explicit: path(&self.config_root),
        }
    }

    /// The full `Roots` a `MergeRoot::Host`-stamped plan entry actually
    /// merges into (`merge/mod.rs`'s `entry_roots`) — as opposed to
    /// [`satisfaction_root`](Roots::satisfaction_root), which only gives a
    /// bare path for checking whether one is already satisfied.
    ///
    /// Two different answers depending on privilege:
    /// - `--root` (privileged offset, portage `ROOT=` parity): the real host
    ///   `/`, same as `root_set().broot()` — an unsatisfied Host-routed
    ///   BDEPEND installs there because the invocation has root to do so.
    /// - `--prefix` (unprivileged overlay): the prefix itself
    ///   (`outer_roots()`, whose `merge_root()` is already the promoted
    ///   prefix-target view) — the overlay cannot write the real host `/`,
    ///   so an unsatisfied BDEPEND must land in the prefix instead. Only the
    ///   *satisfaction check* (is it already present) stays host-anchored,
    ///   via `satisfaction_root`/`is_overlay`'s VDB-weave callers.
    /// - `--local`/bare: BROOT already equals the merge root, so the two
    ///   questions coincide.
    pub(crate) fn broot(&self) -> Roots {
        let base = self.base_roots();
        if base.is_overlay() {
            return self.outer_roots();
        }
        let broot = self.root_set().broot().to_owned();
        Roots {
            config: base.config.clone(),
            base: Some(broot.clone()),
            target: Some(broot),
            broot: base.broot.clone(),
            is_cross_arch: base.is_cross_arch,
            eprefix: base.eprefix.clone(),
            config_overlay: base.config_overlay.clone(),
            relocate: base.relocate,
            config_root_explicit: base.config_root_explicit.clone(),
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
            return main
                .location
                .as_path()
                .map(|p| p.to_path_buf())
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
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
            Ok(rc) if !rc.repos().is_empty() => rc
                .repos()
                .iter()
                .filter_map(|e| e.location.as_path().map(std::path::PathBuf::from))
                .collect(),
            _ => vec![std::path::PathBuf::from("/var/db/repos/gentoo")],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_targets_sysroot_under_eroot() {
        // `--target` sits under the `--root` EROOT and pins config == base ==
        // target to `<EROOT>/usr/<tuple>` (PORTAGE_CONFIGROOT == ROOT == SYSROOT).
        let cli = Cli::parse_from([
            "em",
            "--root",
            "/srv/x",
            "--target",
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
        let cli = Cli::parse_from(["em", "--target", "riscv64-unknown-linux-gnu", "-p", "zlib"]);
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

    /// `--prefix` is an unprivileged overlay: it cannot write the real host
    /// `/`, so an unsatisfied `MergeRoot::Host` plan entry (`entry_roots()`
    /// in `merge/mod.rs`, fed by `Cli::broot()`) must merge into the prefix
    /// instead — unlike `--root`, where the same entry correctly lands on
    /// the real host because that invocation has root. `broot()`'s `.broot`
    /// field (the *satisfaction* root) stays the host either way; only the
    /// merge destination (`merge_root()`) differs here.
    #[test]
    fn prefix_overlay_broot_merges_into_prefix_not_host() {
        let cli = Cli::parse_from(["em", "--prefix", "/opt/p", "-p", "sys-libs/zlib"]);
        let broot = cli.broot();
        assert_eq!(
            broot.merge_root().as_str(),
            "/opt/p",
            "an unsatisfied Host-routed BDEPEND must merge into the prefix under --prefix"
        );
        assert_eq!(
            broot.satisfaction_root(DepClass::Bdepend).as_str(),
            "/",
            "BDEPEND satisfaction must still be checked against the host under --prefix"
        );
    }

    /// Portage `ROOT=`/`{target}-emerge` parity: `--root R`'s BROOT is the
    /// real host `/`, not `R`. `R` only receives the *install*; BDEPEND
    /// tools run against (and are checked against) the host, exactly like
    /// `--prefix`. Previously `base_roots().merge_root()` was (mis)used for
    /// this and returned `R`, making an offset build check BDEPEND against
    /// the (usually near-empty) offset VDB instead of the host's —
    /// `roots().satisfaction_root(DepClass::BDepend)` is the dedicated
    /// accessor now; `base_roots()` keeps its own, different "outer EROOT"
    /// meaning (see both their doc comments). See todo/root-topology-refactor.md.
    #[test]
    fn root_broot_is_host_not_offset() {
        let cli = Cli::parse_from(["em", "--root", "/srv/x", "-p", "sys-libs/zlib"]);
        assert_eq!(
            cli.roots().satisfaction_root(DepClass::Bdepend).as_str(),
            "/",
            "roots().satisfaction_root(BDepend) must be the host under --root"
        );
        assert_eq!(
            cli.base_roots().merge_root().as_str(),
            "/srv/x",
            "base_roots().merge_root() (outer EROOT) must stay the offset under --root"
        );
        assert_eq!(
            cli.roots().merge_root().as_str(),
            "/srv/x",
            "roots().merge_root() (install target) must stay the offset under --root"
        );
    }

    /// `--local DIR` uses `DIR` directly as the standalone prefix root (not
    /// `DIR/.gentoo` — that expansion only applies to the bare-flag default,
    /// covered by `local_is_standalone_not_overlay`).
    #[test]
    fn local_with_path_uses_dir_directly() {
        let cli = Cli::parse_from(["em", "--local", "/tmp/x", "-p", "sys-libs/zlib"]);
        let r = cli.base_roots();
        assert_eq!(r.base().unwrap().as_str(), "/tmp/x");
        assert_eq!(r.target().unwrap().as_str(), "/tmp/x");
        assert_eq!(r.eprefix().unwrap().as_str(), "/tmp/x");
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
        /// The resolved plan entry's authoritative cpv — see
        /// `privilege::WorkerArgs::cpv`.
        #[arg(long)]
        cpv: String,
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

    /// Build an extra package onto the established cross target (may be
    /// given multiple times). `CATEGORY/PN` — crossdev's own `--ex-pkg`: it
    /// always runs on the host (like `binutils`/`gcc`), not the target
    /// sysroot, matching real crossdev's `set_env` treatment of `--ex-pkg`
    /// extras. Applies to `--init-target`/`--setup` only (a config-time
    /// concern, not a build one); named per invocation, like real crossdev —
    /// not remembered across a later run that omits it.
    #[arg(long, value_name = "CATEGORY/PN")]
    pub ex_pkg: Vec<String>,

    /// Build a cross gdb (`dev-debug/gdb`) — shorthand for `--ex-pkg
    /// dev-debug/gdb`, crossdev's own `--ex-gdb`.
    #[arg(long)]
    pub ex_gdb: bool,

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
