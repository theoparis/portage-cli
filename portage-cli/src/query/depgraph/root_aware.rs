//! Root-aware merge planning for cross-compilation (crossdev / `{target}-emerge`).
//!
//! Stage 3b: dual `(package, merge_root)` solver nodes live in
//! `portage-atom-pubgrub`; this module handles cross-context detection and
//! plan/output glue.

use camino::{Utf8Path, Utf8PathBuf};
use gentoo_core::Arch;
use portage_atom::Version;
use portage_atom_pubgrub::{MergeRoot, PortagePackage};

use crate::cli::Roots;

/// Cross-compilation context derived from CLI roots.
///
/// The single owner of "is this a cross build, and how" for the resolver: the
/// derived predicates ([`is_cross_arch`](Self::is_cross_arch),
/// [`target_arch`](Self::target_arch), [`root_deps_rdeps`](Self::root_deps_rdeps))
/// are computed here so call sites don't each re-derive them from `chost`.
#[derive(Debug, Clone)]
pub struct CrossContext {
    /// Whether dual-root cross planning is active for this invocation (any of:
    /// config≠install root, foreign target arch, or an install offset).
    pub active: bool,
    /// `ESYSROOT` / `PORTAGE_CONFIGROOT`: where `DEPEND` is resolved.
    pub sysroot: Utf8PathBuf,
    /// `ROOT` / `EROOT`: where target packages install.
    pub target: Utf8PathBuf,
    /// Target `CHOST` from the profile `make.conf` (if readable).
    pub chost: Option<String>,
    /// Host `CBUILD` from the profile `make.conf` (if readable).
    pub cbuild: Option<String>,
    /// Gentoo keyword `ARCH` of the target `CHOST` (e.g. `riscv`), when `active`
    /// and the `CHOST` maps to a known arch. Derived once; drives both keyword
    /// acceptance and the `--root-deps=rdeps` gate.
    target_arch: Option<Arch>,
}

impl CrossContext {
    /// `true` when the target profile declares a different machine than the host.
    pub fn is_cross_arch(&self) -> bool {
        match (self.chost.as_deref(), self.cbuild.as_deref()) {
            (Some(c), Some(b)) => c != b,
            _ => self.sysroot.as_str() != "/",
        }
    }

    /// The target keyword arch (from `CHOST`), if this is an active cross build
    /// to a recognised arch. Used to accept the target's keywords instead of the
    /// host `--arch`.
    pub fn target_arch(&self) -> Option<&Arch> {
        self.target_arch.as_ref()
    }

    /// Whether crossdev `--root-deps=rdeps` applies: a genuine cross-*arch* build
    /// (target arch differs from the invocation's `host_arch`). Same-arch offset/
    /// stage builds return `false` and keep `DEPEND` → target ROOT.
    pub fn root_deps_rdeps(&self, host_arch: &Arch) -> bool {
        self.target_arch.as_ref().is_some_and(|ta| ta != host_arch)
    }
}

/// Detect cross context from CLI roots (no flag required).
pub fn detect(roots: &Roots) -> CrossContext {
    let sysroot = roots
        .sysroot()
        .map(|p| p.to_owned())
        .unwrap_or_else(|| Utf8PathBuf::from("/"));
    let target = roots.merge_root().to_owned();
    let dual_root = sysroot.as_str() != target.as_str();
    let offset_build = target.as_str() != "/";
    let (chost, cbuild) = read_chost_cbuild(&sysroot);
    let cross_arch = match (chost.as_deref(), cbuild.as_deref()) {
        (Some(c), Some(b)) => c != b,
        _ => false,
    };

    // Active for crossdev, config≠merge offsets (`--config-root / --root stage1/`),
    // and native stage/offset builds (`--root stage1/`) so BDEPEND/IDEPEND route to
    // BROOT with `(package, merge_root)` solver nodes.
    if !dual_root && !cross_arch && !offset_build {
        return CrossContext {
            active: false,
            sysroot: Utf8PathBuf::from("/"),
            target: Utf8PathBuf::from("/"),
            chost: None,
            cbuild: None,
            target_arch: None,
        };
    }

    let target_arch = chost.as_deref().and_then(Arch::from_chost);
    CrossContext {
        active: true,
        sysroot,
        target,
        chost,
        cbuild,
        target_arch,
    }
}

/// One line of the merge list with an explicit merge destination.
#[derive(Debug, Clone)]
pub struct PlanEntry {
    pub pkg: PortagePackage,
    pub version: Version,
    pub merge_root: MergeRoot,
}

/// Map solver install order to plan entries (merge root from solver identity).
pub fn build_plan(target_order: Vec<(PortagePackage, Version)>) -> Vec<PlanEntry> {
    target_order
        .into_iter()
        .map(|(pkg, ver)| PlanEntry {
            merge_root: pkg.merge_root(),
            pkg,
            version: ver,
        })
        .collect()
}

/// Display path for emerge-style ` to <path>/` annotations.
pub fn display_root<'a>(
    merge_root: MergeRoot,
    target: &'a Utf8Path,
    cross: &CrossContext,
) -> &'a Utf8Path {
    match merge_root {
        MergeRoot::Host => Utf8Path::new("/"),
        MergeRoot::Target => {
            if cross.active {
                target
            } else {
                Utf8Path::new("/")
            }
        }
    }
}

fn read_chost_cbuild(root: &Utf8Path) -> (Option<String>, Option<String>) {
    let var =
        |mc: &portage_repo::MakeConf, k| mc.get(k).filter(|s| !s.is_empty()).map(str::to_owned);
    for rel in ["etc/portage/make.conf", "etc/make.conf"] {
        if let Ok(mc) = portage_repo::MakeConf::load(&root.join(rel)) {
            let (chost, cbuild) = (var(&mc, "CHOST"), var(&mc, "CBUILD"));
            if chost.is_some() || cbuild.is_some() {
                return (chost, cbuild);
            }
        }
    }
    (None, None)
}
