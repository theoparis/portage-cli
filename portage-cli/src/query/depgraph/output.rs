use std::collections::HashMap;
use std::io::Write as _;

use anstyle::{AnsiColor, Effects, Style};
use portage_atom::interner::Interned;
use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{
    DepClass, PortagePackage, PortageVersionSet, UseConfig, UseFlagRequirement, UseFlagState,
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

pub(super) fn report_dropped_deps(
    dropped: &[(PortagePackage, PortageVersionSet)],
    data: &RepoData,
    arch: &str,
) {
    let non_virtual: Vec<_> = dropped
        .iter()
        .filter(|(pkg, _)| !pkg.is_virtual())
        .collect();

    if non_virtual.is_empty() {
        return;
    }

    let mut truly_missing: std::collections::BTreeSet<String> = Default::default();
    let mut arch_filtered: std::collections::BTreeSet<String> = Default::default();

    for (pkg, _) in &non_virtual {
        let cpn_str = pkg.cpn().to_string();
        if data.versions.contains_key(pkg.cpn()) {
            arch_filtered.insert(cpn_str);
        } else {
            truly_missing.insert(cpn_str);
        }
    }

    if !truly_missing.is_empty() {
        eprintln!(
            "warning: {} package(s) not found in repo: {}",
            truly_missing.len(),
            truly_missing.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    if !arch_filtered.is_empty() {
        eprintln!(
            "note: {} package(s) skipped (no keywords for {}): {}",
            arch_filtered.len(),
            arch,
            arch_filtered.into_iter().collect::<Vec<_>>().join(", ")
        );
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
