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

/// The planner's "installed" view: `VDB(base) ∪ VDB(target)`, target shadowing
/// base on a duplicate cpv (see docs/root-model.md). Target is loaded first so
/// it wins; when `base == target` (full-offset / host) this collapses to one
/// VDB. `None` means the host `/var/db/pkg`.
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
