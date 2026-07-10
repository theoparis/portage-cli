use std::collections::HashMap;

use portage_atom::DepEntry;
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Version};
use portage_atom_pubgrub::PortagePackage;
use portage_vdb::Vdb;

pub(super) struct VdbEntry {
    pub(super) cpn: Cpn,
    pub(super) slot: Option<String>,
    pub(super) version: Version,
    pub(super) active_use: Vec<Interned<DefaultInterner>>,
    pub(super) iuse: Vec<Interned<DefaultInterner>>,
    /// RDEPEND + DEPEND as stored in the VDB (pre-USE evaluation).
    pub(super) deps: Vec<DepEntry>,
}

/// Installed view for **ROOT** / RDEPEND / merge filtering / action tags.
///
/// See docs/root-model.md: host-config stage uses `VDB(target)` only; prefix
/// overlay uses `VDB(base) ∪ VDB(target)`; host uses `VDB(/)`.
///
/// `--emptytree` does **not** clear this view — emerge still reads the VDB for
/// action tags and post-solve checks; only package *selection* changes (see
/// `InstalledPolicy::Rebuild` in the solver).
pub(super) fn load_target_installed(roots: &crate::cli::Roots) -> Vec<VdbEntry> {
    let base = roots.base();
    let target = roots.target();
    if base != target {
        return load_installed(base, target);
    }
    load_one(target.or(base))
}

/// Union of two VDB roots with target shadowing base (prefix / general overlay).
/// `None` means the host `/var/db/pkg`.
pub(super) fn load_installed(
    base: Option<&camino::Utf8Path>,
    target: Option<&camino::Utf8Path>,
) -> Vec<VdbEntry> {
    let mut roots = vec![target];
    if target != base {
        roots.push(base);
    }
    let mut seen: std::collections::HashSet<(Cpn, String)> = std::collections::HashSet::new();
    let mut out: Vec<VdbEntry> = Vec::new();
    for root in roots {
        for entry in load_one(root) {
            if seen.insert((entry.cpn, entry.version.to_string())) {
                out.push(entry);
            }
        }
    }
    out
}

/// A package present on the build host (BROOT): the host instance's slot-resolved
/// package, version, and VDB-recorded active USE / IUSE. The USE/IUSE let the
/// solver check an edge's atom USE-deps against the host, so a `[flag]` the host
/// lacks triggers a rebuild rather than being pruned as host-satisfied.
pub(super) struct HostInstalledEntry {
    pub package: PortagePackage,
    pub version: Version,
    pub active_use: Vec<Interned<DefaultInterner>>,
    pub iuse: Vec<Interned<DefaultInterner>>,
}

/// Packages present on the **build host** (BROOT) for `host_installed` — a
/// BDEPEND already present there is satisfied without building it, unless a
/// USE-dep on that edge demands a flag the host lacks (in which case the
/// package is rebuilt). Duplicates across slots of the same package are kept
/// (each slot is a distinct `PortagePackage`).
///
/// The root selection (BROOT, plus the prefix's own VDB under `--prefix`) is
/// `crate::bdepend_avail::broot_vdb_packages` — shared with
/// `Avail::initial_bdepend`, which the same #28/#30 bug (reading the bare
/// host `/var/db/pkg` instead of the given BROOT) was once fixed in
/// separately; see that function's doc comment for the full rationale.
/// `add_host_installed` (`provider/mod.rs`) does a plain `HashMap::insert`
/// keyed by package, so whichever entry is appended last wins — matching
/// "what is in the prefix drives" for a package present in both (host
/// entries come first, prefix second).
pub(super) fn load_host_installed(roots: &crate::cli::Roots) -> Vec<HostInstalledEntry> {
    crate::bdepend_avail::broot_vdb_packages(roots)
        .into_iter()
        .map(|pkg| {
            let slot = pkg.slot_main().ok();
            let package = match slot.as_deref().filter(|s| !s.is_empty()) {
                Some(s) => PortagePackage::slotted(*pkg.cpn(), Interned::intern(s)),
                None => PortagePackage::unslotted(*pkg.cpn()),
            };
            let active_use = pkg
                .use_flags()
                .unwrap_or_default()
                .into_iter()
                .map(|f| Interned::intern(&f))
                .collect();
            let iuse = pkg
                .iuse()
                .unwrap_or_default()
                .into_iter()
                .map(|f| Interned::intern(f.trim_start_matches(['+', '-'])))
                .collect();
            HostInstalledEntry {
                package,
                version: pkg.cpv().version.clone(),
                active_use,
                iuse,
            }
        })
        .collect()
}

/// VDB entries from a cross sysroot (`ESYSROOT`) for `DEPEND` satisfaction.
pub(super) fn load_sysroot_entries(sysroot: &camino::Utf8Path) -> Vec<VdbEntry> {
    load_one(Some(sysroot))
}

fn load_one(root: Option<&camino::Utf8Path>) -> Vec<VdbEntry> {
    let vdb = match root {
        Some(r) => Vdb::open(r.join("var/db/pkg")),
        None => Vdb::open_default(),
    };
    let Ok(vdb) = vdb else {
        return Vec::new();
    };
    vdb.packages()
        .into_iter()
        .map(|pkg| {
            let active_use = pkg
                .use_flags()
                .unwrap_or_default()
                .into_iter()
                .map(|f| Interned::intern(&f))
                .collect();
            let iuse = pkg
                .iuse()
                .unwrap_or_default()
                .into_iter()
                .map(|f| Interned::intern(f.trim_start_matches(['+', '-'])))
                .collect();
            let mut deps: Vec<DepEntry> = Vec::new();
            for field in [pkg.rdepend(), pkg.depend()] {
                if let Ok(Some(entries)) = field {
                    deps.extend(entries);
                }
            }
            VdbEntry {
                cpn: *pkg.cpn(),
                slot: pkg.slot_main().ok(),
                version: pkg.cpv().version.clone(),
                active_use,
                iuse,
                deps,
            }
        })
        .collect()
}

/// Determine the emerge-style action tag and the currently-installed version
/// for a given (package, candidate version) pair.
///
/// - `("N",  None)`     — not installed at all
/// - `("NS", None)`     — not in this slot, but other slots are installed
/// - `("U",  Some(v))`  — upgrade within this slot
/// - `("D",  Some(v))`  — downgrade within this slot
/// - `("R",  Some(v))`  — same version, rebuild needed (changed USE flags)
pub(super) fn action_tag<'a>(
    pkg: &PortagePackage,
    ver: &Version,
    installed: &'a HashMap<Cpn, HashMap<String, Version>>,
) -> (&'static str, Option<&'a Version>) {
    let Some(by_slot) = installed.get(pkg.cpn()) else {
        return ("N", None);
    };
    let slot_key = pkg
        .slot()
        .map(|s| s.as_str().to_string())
        .unwrap_or_default();
    match by_slot.get(&slot_key) {
        None => ("NS", None),
        Some(inst) => {
            let tag = if ver > inst {
                "U"
            } else if ver < inst {
                "D"
            } else {
                "R"
            };
            (tag, Some(inst))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the riscv64 stage3 shakeout (#28/#30): a Host
    /// BDEPEND rebuilt into `base_roots()` must be recognized as satisfied
    /// by reading *that* root's VDB, not the bare host `/var/db/pkg`.
    #[test]
    fn load_host_installed_reads_the_given_root_not_the_bare_host() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg_dir = tmp.path().join("var/db/pkg/dev-python/jinja2-3.1.6");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("EAPI"), "8").unwrap();
        std::fs::write(pkg_dir.join("SLOT"), "0").unwrap();
        std::fs::write(pkg_dir.join("CONTENTS"), "").unwrap();
        std::fs::write(
            pkg_dir.join("USE"),
            "python_targets_python3_14 python_targets_python3_13",
        )
        .unwrap();

        let root_str = tmp.path().to_str().unwrap();
        let host_roots = crate::cli::Roots::for_test(root_str);
        let entries = load_host_installed(&host_roots);

        assert_eq!(
            entries.len(),
            1,
            "should find the package in the given root's VDB, not the bare host's"
        );
        assert!(
            entries[0]
                .active_use
                .iter()
                .any(|f| f.as_str() == "python_targets_python3_14"),
            "USE flags must come from the given root's VDB entry"
        );
    }

    fn write_fake_vdb_entry(root: &std::path::Path, cpv: &str, use_flags: &str) {
        let pkg_dir = root.join("var/db/pkg").join(cpv);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("EAPI"), "8").unwrap();
        std::fs::write(pkg_dir.join("SLOT"), "0").unwrap();
        std::fs::write(pkg_dir.join("CONTENTS"), "").unwrap();
        std::fs::write(pkg_dir.join("USE"), use_flags).unwrap();
    }

    /// `--prefix`: `load_host_installed` must weave in the prefix's own VDB
    /// (not just the host's), and the prefix's entry must win when both
    /// have the package — matching "what is in the prefix drives", since an
    /// unsatisfied BDEPEND now merges into the prefix, never the real host.
    #[test]
    fn load_host_installed_weaves_prefix_over_host_under_overlay() {
        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        write_fake_vdb_entry(
            host.path(),
            "dev-python/jinja2-3.1.6",
            "python_targets_python3_13",
        );
        write_fake_vdb_entry(
            prefix.path(),
            "dev-python/jinja2-3.1.6",
            "python_targets_python3_14",
        );

        let roots = crate::cli::Roots::for_test_overlay(
            host.path().to_str().unwrap(),
            prefix.path().to_str().unwrap(),
        );
        let entries = load_host_installed(&roots);

        // Host is read first, prefix second: not deduplicated here (the
        // caller's `HashMap::insert` per entry, in order, is what makes the
        // last one — the prefix's — win; see `add_host_installed`).
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .last()
                .unwrap()
                .active_use
                .iter()
                .any(|f| f.as_str() == "python_targets_python3_14"),
            "the prefix's entry must be read last, so it wins once inserted by package key"
        );
    }

    /// A package present only on the host (never built into the prefix)
    /// must still be found — the overlay weave adds the prefix, it doesn't
    /// replace the host.
    #[test]
    fn load_host_installed_still_finds_host_only_entry_under_overlay() {
        let host = tempfile::tempdir().unwrap();
        let prefix = tempfile::tempdir().unwrap();
        write_fake_vdb_entry(host.path(), "dev-python/jinja2-3.1.6", "");

        let roots = crate::cli::Roots::for_test_overlay(
            host.path().to_str().unwrap(),
            prefix.path().to_str().unwrap(),
        );
        let entries = load_host_installed(&roots);

        assert_eq!(entries.len(), 1, "must still find the host-only entry");
    }
}
