//! Root-aware merge planning for cross-compilation (crossdev / `{target}-emerge`).
//!
//! Stage 3b: dual `(package, merge_root)` solver nodes live in
//! `portage-atom-pubgrub`; this module handles cross-context detection and
//! plan/output glue.

use camino::{Utf8Path, Utf8PathBuf};
use gentoo_core::Arch;
use portage_atom::Version;
use portage_atom_pubgrub::{MergeRoot, PortagePackage};

use crate::Roots;

/// Cross-compilation context derived from CLI roots.
///
/// The single owner of "is this a cross build, and how" for the resolver: the
/// derived predicates ([`is_cross_arch`](Self::is_cross_arch),
/// [`target_arch`](Self::target_arch)) are computed here so call sites don't
/// each re-derive them from `chost`. `--root-deps=rdeps` is deliberately NOT
/// derived here — it's a property of which *operation* is running (`em
/// crossdev --setup` vs `em stages --stage1`), not of the sysroot's
/// CHOST/CBUILD; see `DepgraphOpts::root_deps_rdeps`.
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
    /// and the `CHOST` maps to a known arch. Derived once; drives keyword
    /// acceptance for the target.
    target_arch: Option<Arch>,
    /// Where a `MergeRoot::Host` entry actually lands (mirrors `Cli::broot()`):
    /// the prefix under `--prefix` (an unprivileged overlay can't write the
    /// real host `/`), else the real host `/`. Used by [`display_root`] so
    /// the `-p` merge list matches where the merge actually goes.
    host_target: Utf8PathBuf,
}

impl CrossContext {
    /// `true` when the target profile declares a different machine than the
    /// host. When CHOST/CBUILD can't be read (no sysroot config yet, or a
    /// same-arch offset that never declares them), default to same-arch —
    /// NOT `sysroot != "/"`, which used to treat *any* non-host sysroot
    /// (including a plain same-arch `--root <dir>`) as foreign-arch. Mirrors
    /// `detect()`'s own `cross_arch` local (which already used `_ => false`),
    /// an inconsistency this method used to diverge from. Found 2026-07-11:
    /// that false positive made a same-arch offset build's `DEPEND` stay
    /// unconditionally pinned to the target sysroot in `solve.rs` instead of
    /// dropping host-satisfied edges — `em --root <dir> sys-devel/gcc`
    /// pulled 127 packages where real `ROOT=<dir> emerge` pulls 16. See
    /// `todo/root-topology-refactor.md`.
    pub fn is_cross_arch(&self) -> bool {
        match (self.chost.as_deref(), self.cbuild.as_deref()) {
            (Some(c), Some(b)) => c != b,
            _ => false,
        }
    }

    /// The target keyword arch (from `CHOST`), if this is an active cross build
    /// to a recognised arch. Used to accept the target's keywords instead of the
    /// host `--arch`.
    pub fn target_arch(&self) -> Option<&Arch> {
        self.target_arch.as_ref()
    }
}

/// Detect cross context from CLI roots (no flag required). `host_merge_root`
/// is `Cli::broot()`'s `merge_root()` — passed in rather than derived from
/// `roots.is_overlay()` here, because `roots` can be `--target`-substituted
/// (its `eprefix`/overlay-ness cleared), which would wrongly report the real
/// host as the destination for a `MergeRoot::Host` entry even under an
/// unprivileged `--prefix` overlay (`Cli::broot()` stays overlay-aware
/// regardless of `--target`, since it's derived from `base_roots()`).
pub fn detect(roots: &Roots, host_merge_root: &Utf8Path) -> CrossContext {
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
    let host_target = host_merge_root.to_owned();

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
            host_target,
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
        host_target,
    }
}

/// One line of the merge list with an explicit merge destination.
#[derive(Debug, Clone)]
pub struct PlanEntry {
    /// The solved package identity.
    pub pkg: PortagePackage,
    /// The version to merge.
    pub version: Version,
    /// Where it merges (host BROOT or the target).
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
    cross: &'a CrossContext,
) -> &'a Utf8Path {
    match merge_root {
        MergeRoot::Host => cross.host_target.as_path(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `--prefix`: a `MergeRoot::Host` entry must display as landing in the
    /// prefix, not the real host — matching `Cli::broot()`'s merge
    /// destination fix. Before that fix `display_root` hardcoded `/` here,
    /// which stayed silently correct only because `Cli::broot()` itself
    /// used to be host-anchored for every topology.
    #[test]
    fn host_entry_displays_as_landing_in_the_prefix_under_overlay() {
        let roots = crate::Roots::for_test_overlay("/", "/opt/p");
        let cross = detect(&roots, Utf8Path::new("/opt/p"));
        assert!(cross.active);
        assert_eq!(
            display_root(MergeRoot::Host, &cross.target, &cross).as_str(),
            "/opt/p"
        );
    }

    /// `--root`: a `MergeRoot::Host` entry still displays as landing on the
    /// real host `/` — unaffected by the overlay-only display fix.
    #[test]
    fn host_entry_displays_as_landing_on_the_real_host_under_offset() {
        let roots = crate::Roots::for_test("/srv/x");
        let cross = detect(&roots, Utf8Path::new("/"));
        assert_eq!(
            display_root(MergeRoot::Host, &cross.target, &cross).as_str(),
            "/"
        );
    }

    /// The combined `--prefix --target` case: `roots` here would be
    /// `--target`-substituted (eprefix cleared), but `host_merge_root` is
    /// passed independently (from `Cli::broot()`, unaffected by that
    /// substitution) — the whole point of not deriving `host_target` from
    /// `roots.is_overlay()` inside `detect`.
    #[test]
    fn host_entry_displays_as_landing_in_the_prefix_even_when_roots_is_target_substituted() {
        let sysroot_roots = crate::Roots::for_test("/opt/p/usr/riscv64-unknown-linux-gnu");
        let cross = detect(&sysroot_roots, Utf8Path::new("/opt/p"));
        assert_eq!(
            display_root(MergeRoot::Host, &cross.target, &cross).as_str(),
            "/opt/p"
        );
    }
}
