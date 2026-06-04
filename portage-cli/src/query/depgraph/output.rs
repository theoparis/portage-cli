use std::collections::HashMap;
use std::io::Write as _;

use anstyle::{AnsiColor, Effects, Style};
use portage_atom::interner::Interned;
use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{
    DepClass, DroppedDep, PortagePackage, UseConfig, UseFlagRequirement, UseFlagState,
    apply_package_use,
};
use portage_metadata::CacheEntry;

// emerge color scheme: bold green for keywords/atoms/tags, bold red/blue for flags
const C_PKG: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)))
    .effects(Effects::BOLD);
const C_ON: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)))
    .effects(Effects::BOLD);
const C_OFF: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Blue)))
    .effects(Effects::BOLD);

use super::installed::action_tag;
use super::repo::{RepoData, find_cache};

pub(super) fn report_conflicts(conflicts: &[super::conflicts::Conflict]) {
    use std::collections::BTreeMap;
    // Group by the package whose version is in conflict.
    let mut by_target: BTreeMap<String, Vec<&super::conflicts::Conflict>> = BTreeMap::new();
    for c in conflicts {
        by_target
            .entry(c.dep.cpn.to_string())
            .or_default()
            .push(c);
    }
    let mut out = anstream::stderr();
    writeln!(out, "\n{C_OFF}!!!{C_OFF:#} Slot conflict(s) detected:\n").ok();
    for (target, cs) in &by_target {
        writeln!(out, "  {C_PKG}{target}{C_PKG:#}").ok();
        for c in cs {
            writeln!(
                out,
                "    ({C_PKG}{}-{}{C_PKG:#}, installed) requires {C_OFF}{}{C_OFF:#}",
                c.installed_cpn, c.installed_ver, c.dep,
            ).ok();
            writeln!(
                out,
                "    proposed: {C_PKG}{target}-{}{C_PKG:#}",
                c.proposed_ver,
            ).ok();
        }
        writeln!(out).ok();
    }
}

pub(super) fn report_dropped_deps(dropped: &[DroppedDep], data: &RepoData, arch: &str) {
    // These are || alternatives bypassed by resolution — not failures.
    // Deduplicate by package and merge their alternatives across all occurrences.
    use std::collections::BTreeMap;
    let mut by_pkg: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
    let mut in_tree: std::collections::HashMap<String, bool> = Default::default();

    for d in dropped {
        if d.package.is_virtual() {
            continue;
        }
        let pkg_str = d.package.cpn_str();
        let alts = by_pkg.entry(pkg_str.clone()).or_default();
        for a in &d.alternatives {
            if !a.is_virtual() {
                alts.insert(a.cpn_str());
            }
        }
        in_tree
            .entry(pkg_str)
            .or_insert_with(|| data.versions.contains_key(d.package.cpn()));
    }

    for (pkg_str, alts) in &by_pkg {
        let reason = if *in_tree.get(pkg_str.as_str()).unwrap_or(&false) {
            format!("no {arch} keywords")
        } else {
            "not in tree".to_string()
        };
        let alt_str = if alts.is_empty() {
            String::new()
        } else {
            format!(", alternatives: {}", alts.iter().cloned().collect::<Vec<_>>().join(" | "))
        };
        eprintln!("note: dropped {pkg_str} ({reason}){alt_str}");
    }
}

fn format_flags(
    cache: &CacheEntry,
    use_config: &UseConfig,
    use_expand: &[String],
    use_expand_hidden: &[String],
    is_reinstall: bool,
    req: Option<&UseFlagRequirement>,
) -> String {
    // Each entry: (enabled_tokens, disabled_tokens).  BTreeMap keeps groups sorted.
    let mut base_flags: (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut expand_groups: std::collections::BTreeMap<&str, (Vec<String>, Vec<String>)> =
        std::collections::BTreeMap::new();

    let mut flags: Vec<_> = cache.metadata.iuse.iter().collect();
    flags.sort_by_key(|f| f.name());
    flags.dedup_by_key(|f| f.name());

    for f in flags {
        let name = f.name();
        let iuse_default_enabled =
            matches!(f.default, Some(portage_metadata::IUseDefault::Enabled));
        let interned = Interned::intern(name);
        let mut enabled = match use_config.get_opt(&interned) {
            Some(UseFlagState::Enabled) => true,
            Some(_) => false,
            None => iuse_default_enabled,
        };

        // For reinstall packages: show current state with a change marker.
        // For new/upgrade packages: apply required state directly (it will be
        // enforced at build time, so show the intended post-install state).
        let suffix = if is_reinstall {
            if let Some(r) = req {
                if r.required_enabled.contains(&interned) {
                    "*"
                } else if r.required_disabled.contains(&interned) {
                    "%"
                } else {
                    ""
                }
            } else {
                ""
            }
        } else {
            if let Some(r) = req {
                if r.required_enabled.contains(&interned) {
                    enabled = true;
                } else if r.required_disabled.contains(&interned) {
                    enabled = false;
                }
            }
            ""
        };
        let expand_match = use_expand.iter().find(|key| {
            let prefix = format!("{}_", key.to_lowercase());
            name.starts_with(prefix.as_str())
        });

        let paint = |s: String, on: bool| -> String {
            if on {
                format!("{C_ON}{s}{C_ON:#}")
            } else {
                format!("{C_OFF}{s}{C_OFF:#}")
            }
        };

        if let Some(key) = expand_match {
            let prefix = format!("{}_", key.to_lowercase());
            let short = &name[prefix.len()..];
            let bucket = expand_groups.entry(key.as_str()).or_default();
            if enabled {
                bucket.0.push(paint(format!("{short}{suffix}"), true));
            } else {
                bucket.1.push(paint(format!("-{short}{suffix}"), false));
            }
        } else if enabled {
            base_flags.0.push(paint(format!("{name}{suffix}"), true));
        } else {
            base_flags.1.push(paint(format!("-{name}{suffix}"), false));
        }
    }

    let join_bucket = |(on, off): &(Vec<String>, Vec<String>)| -> String {
        on.iter().chain(off.iter()).cloned().collect::<Vec<_>>().join(" ")
    };

    let mut parts = Vec::new();
    let base_str = join_bucket(&base_flags);
    if !base_str.is_empty() {
        parts.push(format!("USE=\"{base_str}\""));
    }
    for (key, bucket) in &expand_groups {
        if use_expand_hidden.iter().any(|h| h == *key) {
            continue;
        }
        parts.push(format!("{}=\"{}\"", key, join_bucket(bucket)));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  {}", parts.join(" "))
    }
}

pub(super) fn print_pretty(
    data: &RepoData,
    order: &[(PortagePackage, Version)],
    installed: &HashMap<Cpn, HashMap<String, Version>>,
    use_config: &UseConfig,
    package_use: &[(Dep, Vec<String>)],
    use_expand: &[String],
    use_expand_hidden: &[String],
    flag_reqs: &HashMap<&PortagePackage, &UseFlagRequirement>,
) {
    let mut out = anstream::stdout();

    writeln!(out, "{C_PKG}These are the packages that would be merged, in order:{C_PKG:#}\n").ok();
    writeln!(out, "Calculating dependencies... done!").ok();

    for (pkg, ver) in order {
        let cpn = pkg.cpn();
        let (tag, old_ver) = action_tag(pkg, ver, installed);
        let req = flag_reqs.get(pkg).copied();
        let is_reinstall = tag == "R";

        let cpv = Cpv::new(*cpn, ver.clone());
        let effective_use = apply_package_use(use_config, &cpv, package_use);
        let flag_str = find_cache(data, pkg, ver)
            .map(|c| format_flags(c, &effective_use, use_expand, use_expand_hidden, is_reinstall, req))
            .unwrap_or_default();

        let old = old_ver.map(|v| format!(" [{}]", v)).unwrap_or_default();
        let pad = " ".repeat(6usize.saturating_sub(tag.len()));
        writeln!(
            out,
            "[{C_PKG}ebuild{C_PKG:#}  {C_PKG}{tag}{C_PKG:#}{pad}] {C_PKG}{cpn}-{ver}{C_PKG:#}{old}{flag_str}",
        ).ok();
    }

    writeln!(out, "\nTotal: {} package(s)", order.len()).ok();
}

fn class_str(c: DepClass) -> &'static str {
    match c {
        DepClass::Depend => "DEPEND",
        DepClass::Rdepend => "RDEPEND",
        DepClass::Bdepend => "BDEPEND",
        DepClass::Pdepend => "PDEPEND",
        DepClass::Idepend => "IDEPEND",
    }
}

pub(super) fn print_json(
    data: &RepoData,
    order: &[(PortagePackage, Version)],
    edges: &[portage_atom_pubgrub::DepEdge],
    installed: &HashMap<Cpn, HashMap<String, Version>>,
    flag_reqs: &HashMap<&PortagePackage, &UseFlagRequirement>,
) {
    let packages: Vec<serde_json::Value> = order
        .iter()
        .map(|(pkg, ver)| {
            let cpn = pkg.cpn();
            let (tag, old_ver) = action_tag(pkg, ver, installed);
            let status = match tag {
                "U" => "upgrade",
                "D" => "downgrade",
                "R" => "reinstall",
                "NS" => "new_slot",
                _ => "new",
            };
            let mut obj = serde_json::json!({
                "atom": format!("{cpn}-{ver}"),
                "cpn": cpn.to_string(),
                "version": ver.to_string(),
                "repo": data.repo_name,
                "status": status,
            });
            if let Some(old) = old_ver {
                obj["old_version"] = serde_json::Value::String(old.to_string());
            }
            if let Some(cache) = find_cache(data, pkg, ver) {
                let slot = &cache.metadata.slot;
                obj["slot"] = serde_json::Value::String(slot.slot.as_str().to_owned());
                if let Some(sub) = slot.subslot {
                    obj["subslot"] = serde_json::Value::String(sub.as_str().to_owned());
                }
                let iuse: Vec<String> = {
                    let mut iuse_flags: Vec<_> = cache.metadata.iuse.iter().collect();
                    iuse_flags.sort_by_key(|f| f.name());
                    iuse_flags
                        .iter()
                        .map(|f| match f.default {
                            Some(portage_metadata::IUseDefault::Enabled) => {
                                format!("+{}", f.name())
                            }
                            _ => format!("-{}", f.name()),
                        })
                        .collect()
                };
                obj["iuse"] = serde_json::json!(iuse);
            }
            if let Some(req) = flag_reqs.get(pkg) {
                if !req.required_enabled.is_empty() {
                    let flags: Vec<&str> =
                        req.required_enabled.iter().map(|f| f.as_str()).collect();
                    obj["required_use_enabled"] = serde_json::json!(flags);
                }
                if !req.required_disabled.is_empty() {
                    let flags: Vec<&str> =
                        req.required_disabled.iter().map(|f| f.as_str()).collect();
                    obj["required_use_disabled"] = serde_json::json!(flags);
                }
            }
            obj
        })
        .collect();

    let dep_edges: Vec<serde_json::Value> = edges
        .iter()
        .map(|e| {
            serde_json::json!({
                "from": format!("{}-{}", e.from.0.cpn(), e.from.1),
                "to": format!("{}-{}", e.to.0.cpn(), e.to.1),
                "class": class_str(e.class),
            })
        })
        .collect();

    let out = serde_json::json!({
        "packages": packages,
        "edges": dep_edges,
    });

    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

const C_DIM: Style = Style::new().effects(Effects::DIMMED);

pub(super) fn print_tree(
    roots: &[(PortagePackage, Version)],
    edges: &[portage_atom_pubgrub::DepEdge],
    installed_cpvs: &std::collections::HashSet<Cpv>,
) {
    // version map: package -> version (from edges, covers all non-virtual nodes)
    let mut version_map: HashMap<&PortagePackage, &Version> = HashMap::new();
    for e in edges {
        version_map.insert(&e.from.0, &e.from.1);
        version_map.insert(&e.to.0, &e.to.1);
    }
    // also insert roots in case they have no outgoing edges
    for (pkg, ver) in roots {
        version_map.entry(pkg).or_insert(ver);
    }

    // children map: package -> ordered list of (package, version) deps
    let mut children: HashMap<&PortagePackage, Vec<(&PortagePackage, &Version)>> = HashMap::new();
    for e in edges {
        let ver = version_map.get(&e.to.0).copied().unwrap_or(&e.to.1);
        children.entry(&e.from.0).or_default().push((&e.to.0, ver));
    }
    // deduplicate children (same package may appear via multiple dep classes)
    for kids in children.values_mut() {
        kids.dedup_by_key(|(pkg, _)| *pkg);
    }

    let mut out = anstream::stdout();
    let mut visited: std::collections::HashSet<*const PortagePackage> = Default::default();

    for (i, (pkg, ver)) in roots.iter().enumerate() {
        let is_last = i == roots.len() - 1;
        tree_node(
            &mut out,
            pkg,
            ver,
            &children,
            installed_cpvs,
            "",
            is_last,
            true,
            &mut visited,
        );
    }
}

fn tree_node(
    out: &mut impl std::io::Write,
    pkg: &PortagePackage,
    ver: &Version,
    children: &HashMap<&PortagePackage, Vec<(&PortagePackage, &Version)>>,
    installed_cpvs: &std::collections::HashSet<Cpv>,
    prefix: &str,
    is_last: bool,
    is_root: bool,
    visited: &mut std::collections::HashSet<*const PortagePackage>,
) {
    let already = !visited.insert(pkg as *const _);
    let cpn = pkg.cpn();
    let is_installed = installed_cpvs.contains(&Cpv::new(*cpn, ver.clone()));

    let connector = if is_root {
        ""
    } else if is_last {
        "└── "
    } else {
        "├── "
    };

    if is_installed {
        writeln!(
            out,
            "{prefix}{connector}{C_DIM}{cpn}-{ver}{C_DIM:#}{}",
            if already { " (*)" } else { "" }
        )
        .ok();
    } else {
        writeln!(
            out,
            "{prefix}{connector}{C_PKG}{cpn}-{ver}{C_PKG:#}{}",
            if already { format!(" {C_DIM}(*){C_DIM:#}") } else { String::new() }
        )
        .ok();
    }

    if already {
        return;
    }

    let kids = children.get(pkg).map(|v| v.as_slice()).unwrap_or(&[]);
    let child_prefix = if is_root {
        prefix.to_string()
    } else if is_last {
        format!("{prefix}    ")
    } else {
        format!("{prefix}│   ")
    };

    for (i, (child, child_ver)) in kids.iter().enumerate() {
        let child_is_last = i == kids.len() - 1;
        tree_node(
            out,
            child,
            child_ver,
            children,
            installed_cpvs,
            &child_prefix,
            child_is_last,
            false,
            visited,
        );
    }
}
