use std::collections::{BinaryHeap, HashMap};

use portage_atom::{Cpn, Version};

use crate::package::PortagePackage;
use crate::provider::PortageDependencyProvider;
use crate::version_set::PortageVersionSet;

/// Dependency class label for an edge in the dependency graph.
///
/// Corresponds to the five dependency variables defined by PMS 8.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DepClass {
    /// DEPEND — build-time dependencies.
    Depend,
    /// RDEPEND — run-time dependencies.
    Rdepend,
    /// BDEPEND — build-host dependencies (EAPI 7+).
    Bdepend,
    /// PDEPEND — post-merge dependencies.
    Pdepend,
    /// IDEPEND — install-time dependencies (EAPI 8+).
    Idepend,
}

/// A labeled edge in the dependency graph: (from_pkg, from_version) depends on
/// (to_pkg, to_version) via the given class.
#[derive(Debug, Clone)]
pub struct DepEdge {
    /// The package that declares the dependency.
    pub from: (PortagePackage, Version),
    /// The package that is depended upon.
    pub to: (PortagePackage, Version),
    /// Which dependency class this edge belongs to.
    pub class: DepClass,
    /// The USE flag in `from` that gates this dep, if it was inside `flag? ( dep )`.
    pub via_use_flag:
        Option<portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>>,
}

impl PortageDependencyProvider {
    /// Build the labeled dependency graph from a solution.
    ///
    /// Returns edges labeled with the dependency class (DEPEND, RDEPEND, etc.).
    /// Only edges where both endpoints are in the solution are included.
    pub fn dependency_graph(
        &self,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    ) -> Vec<DepEdge> {
        let mut edges = Vec::new();
        let classes = [
            DepClass::Depend,
            DepClass::Rdepend,
            DepClass::Bdepend,
            DepClass::Pdepend,
            DepClass::Idepend,
        ];

        // Index solution by CPN so dependency lookups are O(1) instead of O(n).
        // Skip virtual packages (USE-decision nodes, synthetic root) — they
        // have no CPN and must not appear in the output graph.
        let mut by_cpn: HashMap<&Cpn, Vec<(&PortagePackage, &Version)>> = HashMap::new();
        for (sol_pkg, sol_ver) in solution.iter() {
            if sol_pkg.is_virtual() {
                continue;
            }
            by_cpn
                .entry(sol_pkg.cpn())
                .or_default()
                .push((sol_pkg, sol_ver));
        }

        for (pkg, version) in solution.iter() {
            let Some(data) = self.packages.get(pkg) else {
                continue;
            };
            let Some(vd) = data.versions.get(version) else {
                continue;
            };

            for (class_idx, &class) in classes.iter().enumerate() {
                for (dep_pkg, dep_vs, gating_flag) in &vd.by_class[class_idx] {
                    // A dep may point at a virtual choice/slot/use-decision node.
                    // Those are stripped from the solution but remain in
                    // `self.packages`, so expand them transitively to the real
                    // packages they select (filtered to the solution by the
                    // inner version-sets). Without this, deps reachable only
                    // through a USE-conditional / `||` group / multi-slot choice
                    // produce no ordering edge — e.g. `vala? ( || ( dev-lang/vala:0.56 ) )`
                    // left librsvg unordered w.r.t. vala.
                    let mut seen: std::collections::HashSet<&PortagePackage> =
                        std::collections::HashSet::new();
                    let mut emitted: std::collections::HashSet<(&PortagePackage, &Version)> =
                        std::collections::HashSet::new();
                    let mut work: Vec<(&PortagePackage, &PortageVersionSet)> =
                        vec![(dep_pkg, dep_vs)];
                    while let Some((dp, dvs)) = work.pop() {
                        if dp.is_virtual() {
                            if !seen.insert(dp) {
                                continue;
                            }
                            if let Some(vdata) = self.packages.get(dp) {
                                for vver in vdata.versions.values() {
                                    for (idp, idvs, _) in vver.by_class.iter().flatten() {
                                        work.push((idp, idvs));
                                    }
                                }
                            }
                            continue;
                        }
                        let Some(candidates) = by_cpn.get(dp.cpn()) else {
                            continue;
                        };
                        for &(sol_pkg, sol_ver) in candidates {
                            if dvs.contains(sol_ver) && emitted.insert((sol_pkg, sol_ver)) {
                                edges.push(DepEdge {
                                    from: (pkg.clone(), version.clone()),
                                    to: (sol_pkg.clone(), sol_ver.clone()),
                                    class,
                                    via_use_flag: *gating_flag,
                                });
                            }
                        }
                    }
                }
            }
        }

        edges
    }

    /// Compute an installation order from a solution.
    ///
    /// Returns packages in topological order: a dependency is merged before the
    /// package that needs it.  Both build-time (DEPEND/BDEPEND) and runtime
    /// (RDEPEND) edges constrain the order, so e.g. the requested target lands
    /// after the libraries it links and runs against.  PDEPEND (merged *after*
    /// the parent) and IDEPEND (install-time only) do not constrain it.
    ///
    /// RDEPEND introduces cycles far more often than build deps alone (e.g.
    /// `gtk+` ↔ its icon-theme/at-spi runtime deps).  Portage resolves these by
    /// treating runtime edges as *soft*: when the graph stalls in a cycle, soft
    /// edges are dropped to break it while hard build-time edges are preserved.
    /// We do the same — only if a genuine hard (build-time) cycle remains, as
    /// with bootstrap cycles (`xz-utils` ↔ `elt-patches`), do we fall back to a
    /// deterministic lexicographic tie-break.
    pub fn install_order(
        &self,
        solution: &pubgrub::SelectedDependencies<PortagePackage, Version>,
    ) -> Vec<(PortagePackage, Version)> {
        let graph = self.dependency_graph(solution);

        // Index nodes deterministically (sorted by key) so SCC discovery and all
        // tie-breaks are reproducible.
        let mut node_pv: Vec<(String, (PortagePackage, Version))> = solution
            .iter()
            .map(|(pkg, ver)| (format!("{}-{}", pkg, ver), (pkg.clone(), ver.clone())))
            .collect();
        node_pv.sort_by(|a, b| a.0.cmp(&b.0));
        let n = node_pv.len();
        let idx: HashMap<&str, usize> = node_pv
            .iter()
            .enumerate()
            .map(|(i, (k, _))| (k.as_str(), i))
            .collect();

        // Adjacency: dependency → dependent ("dependency comes first").
        // `succ_all` = hard (DEPEND/BDEPEND) + soft (RDEPEND); `succ_hard` only
        // the build-time edges, used to order within a cycle.
        let mut succ_all: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut succ_hard: Vec<Vec<usize>> = vec![Vec::new(); n];
        for edge in &graph {
            let hard = match edge.class {
                DepClass::Depend | DepClass::Bdepend => true,
                DepClass::Rdepend => false,
                // PDEPEND (merged after parent) / IDEPEND: no ordering constraint.
                _ => continue,
            };
            let to = format!("{}-{}", edge.to.0, edge.to.1);
            let from = format!("{}-{}", edge.from.0, edge.from.1);
            let (Some(&u), Some(&v)) = (idx.get(to.as_str()), idx.get(from.as_str())) else {
                continue;
            };
            succ_all[u].push(v);
            if hard {
                succ_hard[u].push(v);
            }
        }
        for adj in succ_all.iter_mut() {
            adj.sort_unstable();
        }

        // Strongly-connected components via iterative Tarjan.  Nodes in different
        // SCCs are linearised by the condensation (a DAG), so every cross-SCC
        // edge — every edge that is not part of a genuine cycle — is respected.
        let comp_of = tarjan_scc(&succ_all);
        let num_comps = comp_of.iter().copied().max().map_or(0, |m| m + 1);
        let mut members: Vec<Vec<usize>> = vec![Vec::new(); num_comps];
        for (node, &c) in comp_of.iter().enumerate() {
            members[c].push(node);
        }

        // Condensation edges + in-degrees (deduplicated).
        let mut comp_succ: Vec<std::collections::BTreeSet<usize>> =
            vec![std::collections::BTreeSet::new(); num_comps];
        let mut comp_indeg = vec![0usize; num_comps];
        for u in 0..n {
            for &v in &succ_all[u] {
                let (cu, cv) = (comp_of[u], comp_of[v]);
                if cu != cv && comp_succ[cu].insert(cv) {
                    comp_indeg[cv] += 1;
                }
            }
        }

        // The component key (max member node key) drives a deterministic
        // max-heap tie-break, preserving the "largest ready first" ordering and
        // keeping the requested target — which has no dependents and so becomes
        // ready last — near the end.
        let comp_key = |c: usize| -> &str {
            members[c]
                .iter()
                .map(|&i| node_pv[i].0.as_str())
                .max()
                .unwrap_or("")
        };

        let mut comp_ready: BinaryHeap<(String, usize)> = (0..num_comps)
            .filter(|&c| comp_indeg[c] == 0)
            .map(|c| (comp_key(c).to_string(), c))
            .collect();

        let mut result = Vec::with_capacity(n);
        while let Some((_, c)) = comp_ready.pop() {
            // Emit this component's members.  A singleton is just itself; a real
            // cycle is ordered internally by breaking soft (RDEPEND) edges before
            // hard ones (see `order_cycle`).
            if members[c].len() == 1 {
                let node = members[c][0];
                result.push(node_pv[node].1.clone());
            } else {
                for node in order_cycle(&members[c], &succ_hard, &succ_all, &node_pv) {
                    result.push(node_pv[node].1.clone());
                }
            }
            for &cv in &comp_succ[c] {
                comp_indeg[cv] -= 1;
                if comp_indeg[cv] == 0 {
                    comp_ready.push((comp_key(cv).to_string(), cv));
                }
            }
        }

        result
    }
}

/// Iterative Tarjan SCC.  Returns the component id of each node; ids are dense
/// `0..num_components`.  `succ[u]` lists nodes that must come *after* `u`.
fn tarjan_scc(succ: &[Vec<usize>]) -> Vec<usize> {
    let n = succ.len();
    let mut index = vec![usize::MAX; n];
    let mut lowlink = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut comp_of = vec![usize::MAX; n];
    let mut next_index = 0usize;
    let mut next_comp = 0usize;

    for s in 0..n {
        if index[s] != usize::MAX {
            continue;
        }
        // DFS frame: (node, next child position).
        let mut call: Vec<(usize, usize)> = vec![(s, 0)];
        while let Some(&mut (v, ref mut ci)) = call.last_mut() {
            if *ci == 0 {
                index[v] = next_index;
                lowlink[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if *ci < succ[v].len() {
                let w = succ[v][*ci];
                *ci += 1;
                if index[w] == usize::MAX {
                    call.push((w, 0));
                } else if on_stack[w] {
                    lowlink[v] = lowlink[v].min(index[w]);
                }
            } else {
                if lowlink[v] == index[v] {
                    loop {
                        let x = stack.pop().unwrap();
                        on_stack[x] = false;
                        comp_of[x] = next_comp;
                        if x == v {
                            break;
                        }
                    }
                    next_comp += 1;
                }
                call.pop();
                if let Some(&(parent, _)) = call.last() {
                    lowlink[parent] = lowlink[parent].min(lowlink[v]);
                }
            }
        }
    }
    comp_of
}

/// Order the members of a single cyclic component.  Every member has an
/// incoming edge from within the cycle, so a plain topological sort is
/// impossible; we break soft (RDEPEND) edges before hard (build-time) ones by
/// repeatedly emitting the member closest to ready — fewest pending hard deps,
/// then fewest pending soft deps, then largest key for determinism.
fn order_cycle(
    members: &[usize],
    succ_hard: &[Vec<usize>],
    succ_all: &[Vec<usize>],
    node_pv: &[(String, (PortagePackage, Version))],
) -> Vec<usize> {
    use std::collections::HashSet;
    let set: HashSet<usize> = members.iter().copied().collect();
    let mut indeg_hard: HashMap<usize, usize> = members.iter().map(|&m| (m, 0)).collect();
    let mut indeg_all: HashMap<usize, usize> = members.iter().map(|&m| (m, 0)).collect();
    for &u in members {
        for &v in &succ_all[u] {
            if set.contains(&v) {
                *indeg_all.get_mut(&v).unwrap() += 1;
            }
        }
        for &v in &succ_hard[u] {
            if set.contains(&v) {
                *indeg_hard.get_mut(&v).unwrap() += 1;
            }
        }
    }

    let mut remaining: HashSet<usize> = set.clone();
    let mut out = Vec::with_capacity(members.len());
    while !remaining.is_empty() {
        let pick = *remaining
            .iter()
            .min_by(|&&a, &&b| {
                let ha = indeg_hard[&a];
                let hb = indeg_hard[&b];
                let aa = indeg_all[&a];
                let ab = indeg_all[&b];
                // Largest key wins ties: compare b before a on the key.
                ha.cmp(&hb)
                    .then(aa.cmp(&ab))
                    .then_with(|| node_pv[b].0.cmp(&node_pv[a].0))
            })
            .unwrap();
        remaining.remove(&pick);
        out.push(pick);
        for &v in &succ_all[pick] {
            if let Some(e) = indeg_all.get_mut(&v) {
                *e = e.saturating_sub(1);
            }
        }
        for &v in &succ_hard[pick] {
            if let Some(e) = indeg_hard.get_mut(&v) {
                *e = e.saturating_sub(1);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{InMemoryRepository, PackageDeps};
    use crate::version_set::PortageVersionSet;
    use portage_atom::interner::{DefaultInterner, Interned};
    use portage_atom::{Cpn, Cpv, Dep, DepEntry};

    #[test]
    fn install_order_and_dependency_graph_work() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: vec![],
            rdepend: vec![],
            bdepend: vec![],
            pdepend: vec![],
            idepend: vec![],
        };

        repo.add_version(
            portage_atom::Cpv::parse("app-misc/top-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: vec![DepEntry::Atom(Dep::parse("dev-libs/bottom-1.0").unwrap())],
                rdepend: vec![],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/bottom-1.0").unwrap(),
            None,
            None,
            empty(),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let top = PortagePackage::unslotted(Cpn::parse("app-misc/top").unwrap());

        let solution = provider
            .resolve_targets(vec![(top, PortageVersionSet::any())])
            .unwrap();

        let edges = provider.dependency_graph(&solution);
        assert!(
            edges.iter().any(|e| e.class == DepClass::Depend),
            "should have a DEPEND edge"
        );

        let order = provider.install_order(&solution);
        let names: Vec<&str> = order
            .iter()
            .map(|(p, _)| p.cpn().package.as_str())
            .collect();
        let bottom_pos = names.iter().position(|&n| n == "bottom").unwrap();
        let top_pos = names.iter().position(|&n| n == "top").unwrap();
        assert!(
            bottom_pos < top_pos,
            "bottom must come before top in install order, got: {:?}",
            names
        );
    }

    #[test]
    fn dependency_graph_returns_labeled_edges() {
        let mut repo = InMemoryRepository::new();
        let empty = || PackageDeps {
            depend: vec![],
            rdepend: vec![],
            bdepend: vec![],
            pdepend: vec![],
            idepend: vec![],
        };

        repo.add_version(
            portage_atom::Cpv::parse("app-misc/app-1.0").unwrap(),
            None,
            None,
            PackageDeps {
                depend: vec![DepEntry::Atom(Dep::parse("dev-libs/lib-1.0").unwrap())],
                rdepend: vec![DepEntry::Atom(Dep::parse("dev-libs/runtime-1.0").unwrap())],
                bdepend: vec![],
                pdepend: vec![],
                idepend: vec![],
            },
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/lib-1.0").unwrap(),
            None,
            None,
            empty(),
        );
        repo.add_version(
            portage_atom::Cpv::parse("dev-libs/runtime-1.0").unwrap(),
            None,
            None,
            empty(),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let app = PortagePackage::unslotted(Cpn::parse("app-misc/app").unwrap());

        let solution = provider
            .resolve_targets(vec![(app, PortageVersionSet::any())])
            .unwrap();
        let edges = provider.dependency_graph(&solution);

        let dep_classes: Vec<_> = edges.iter().map(|e| e.class).collect();
        assert!(
            dep_classes.contains(&DepClass::Depend),
            "should have DEPEND edge"
        );
        assert!(
            dep_classes.contains(&DepClass::Rdepend),
            "should have RDEPEND edge"
        );
    }

    // Integration tests that reproduce the texlive-core → kpathsea scenario:
    // slotted packages, `_p` patch versions, `:=` slot-equals deps, and combined
    // slot+use-dep atoms.  These are the exact forms causing missing transitive
    // deps in the real depgraph (all three parse-level hypotheses were falsified).

    fn slot(s: &str) -> Option<Interned<DefaultInterner>> {
        Some(Interned::intern(s))
    }

    fn rdepend(atoms: &[&str]) -> PackageDeps {
        PackageDeps {
            depend: vec![],
            rdepend: atoms
                .iter()
                .map(|a| DepEntry::Atom(Dep::parse(a).unwrap()))
                .collect(),
            bdepend: vec![],
            pdepend: vec![],
            idepend: vec![],
        }
    }

    #[test]
    fn slotted_dep_via_slot_equals_operator_is_included() {
        // Reproduces: texlive-core has `>=dev-libs/kpathsea-6.4.0:=` in RDEPEND.
        // kpathsea is slotted (SLOT=0/6.4.0) and available at 6.4.0_p20240311-r1.
        // The solver must include kpathsea when resolving texlive-core.
        let mut repo = InMemoryRepository::new();

        repo.add_version(
            Cpv::parse("app-text/texlive-core-2024").unwrap(),
            slot("0"),
            None,
            rdepend(&[">=dev-libs/kpathsea-6.4.0:="]),
        );
        repo.add_version(
            Cpv::parse("dev-libs/kpathsea-6.4.0_p20240311-r1").unwrap(),
            slot("0"),
            Some(Interned::intern("6.4.0")),
            rdepend(&[]),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let target = PortagePackage::slotted(
            Cpn::parse("app-text/texlive-core").unwrap(),
            Interned::intern("0"),
        );

        let solution = provider
            .resolve_targets(vec![(target, PortageVersionSet::any())])
            .unwrap_or_else(|e| panic!("resolution failed: {e:?}"));

        let names: Vec<String> = provider
            .install_order(&solution)
            .into_iter()
            .filter(|(p, _)| !p.is_virtual())
            .map(|(p, _)| p.cpn().to_string())
            .collect();

        assert!(
            names.contains(&"dev-libs/kpathsea".to_string()),
            "kpathsea must be in install_order; got: {names:?}"
        );
        assert!(
            names.contains(&"app-text/texlive-core".to_string()),
            "texlive-core must be in install_order; got: {names:?}"
        );
    }

    #[test]
    fn slot_equals_with_use_deps_included_in_solution() {
        // Reproduces: `>=media-libs/harfbuzz-1.4.5:=[icu,graphite]`
        // The use deps are constraints on the installed harfbuzz, not on the
        // parent package.  harfbuzz must still appear in install_order.
        let mut repo = InMemoryRepository::new();

        repo.add_version(
            Cpv::parse("app-text/texlive-core-2024").unwrap(),
            slot("0"),
            None,
            rdepend(&[">=media-libs/harfbuzz-1.4.5:=[icu,graphite]"]),
        );
        repo.add_version(
            Cpv::parse("media-libs/harfbuzz-12.3.2").unwrap(),
            slot("0"),
            Some(Interned::intern("6.0.0")),
            rdepend(&[]),
        );

        let mut provider = PortageDependencyProvider::new(repo);
        let target = PortagePackage::slotted(
            Cpn::parse("app-text/texlive-core").unwrap(),
            Interned::intern("0"),
        );

        let solution = provider
            .resolve_targets(vec![(target, PortageVersionSet::any())])
            .unwrap_or_else(|e| panic!("resolution failed: {e:?}"));

        let names: Vec<String> = provider
            .install_order(&solution)
            .into_iter()
            .filter(|(p, _)| !p.is_virtual())
            .map(|(p, _)| p.cpn().to_string())
            .collect();

        assert!(
            names.contains(&"media-libs/harfbuzz".to_string()),
            "harfbuzz must be in install_order; got: {names:?}"
        );
    }

    #[test]
    fn versioned_dep_on_p_suffix_version() {
        // `>=dev-libs/kpathsea-6.4.0` must match `6.4.0_p20240311-r1`.
        // VersionSet.contains() must agree with Version's Ord impl.
        use crate::version_set::PortageVersionSet;
        use portage_atom::{Operator, Version};

        let vs = PortageVersionSet::from_operator(
            Operator::GreaterOrEqual,
            false,
            Version::parse("6.4.0").unwrap(),
        );
        for v_str in ["6.4.0_p20240311", "6.4.0_p20240311-r1", "6.5.0"] {
            let v = Version::parse(v_str).unwrap();
            assert!(vs.contains(&v), "VersionSet >=6.4.0 must contain {v_str}");
        }
        for v_str in ["6.3.9", "6.4.0_alpha"] {
            let v = Version::parse(v_str).unwrap();
            assert!(
                !vs.contains(&v),
                "VersionSet >=6.4.0 must NOT contain {v_str}"
            );
        }
    }
}
