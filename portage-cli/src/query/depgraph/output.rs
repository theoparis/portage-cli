use std::collections::HashMap;
use std::io::Write as _;

use anstyle::{AnsiColor, Effects, Style};
use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpn, Cpv, Dep, Version};
use portage_atom_pubgrub::{
    CededFlag, DepClass, DroppedDep, PortagePackage, UseConfig, UseFlagRequirement, UseFlagState,
    apply_package_use,
};
use portage_metadata::CacheEntry;

// emerge color scheme: bold green for keywords/atoms/tags, bold red/blue for flags
// Package names use plain green (not bold) to match portage's PKG_MERGE style
pub(super) const C_PKG: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)));
pub(super) const C_ON: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Red)))
    .effects(Effects::BOLD);
pub(super) const C_OFF: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Blue)))
    .effects(Effects::BOLD);
// Portage-style colors for emerge -p output:
// - BRACKET: blue for [ and ] in [ebuild STATUS]
// - STATUS_N/S: green for new/new-slot
// - STATUS_U: cyan for upgrade
// - STATUS_D: blue for downgrade
// - STATUS_R: yellow for reinstall
pub(super) const C_BRACKET: Style =
    Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Blue)));
pub(super) const C_STATUS_N: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)))
    .effects(Effects::BOLD);
pub(super) const C_STATUS_S: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Green)))
    .effects(Effects::BOLD);
pub(super) const C_STATUS_U: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Cyan)))
    .effects(Effects::BOLD);
pub(super) const C_STATUS_D: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Blue)))
    .effects(Effects::BOLD);
pub(super) const C_STATUS_R: Style = Style::new()
    .fg_color(Some(anstyle::Color::Ansi(AnsiColor::Yellow)))
    .effects(Effects::BOLD);

use super::installed::action_tag;
use super::repo::{RepoData, find_cache};

pub(super) fn report_conflicts(conflicts: &[super::conflicts::Conflict]) {
    use std::collections::BTreeMap;
    // Group by the package whose version is in conflict.
    let mut by_target: BTreeMap<String, Vec<&super::conflicts::Conflict>> = BTreeMap::new();
    for c in conflicts {
        by_target.entry(c.dep.cpn.to_string()).or_default().push(c);
    }
    let mut out = anstream::stderr();
    writeln!(
        out,
        "\n{C_OFF}!!!{C_OFF:#} Dependency constraint conflict(s) detected:\n"
    )
    .ok();
    for (target, cs) in &by_target {
        writeln!(out, "  {C_PKG}{target}{C_PKG:#}").ok();
        for c in cs {
            writeln!(
                out,
                "    ({C_PKG}{}-{}{C_PKG:#}, installed) requires {C_OFF}{}{C_OFF:#}",
                c.installed_cpn, c.installed_ver, c.dep,
            )
            .ok();
            writeln!(
                out,
                "    proposed: {C_PKG}{target}-{}{C_PKG:#}",
                c.proposed_ver,
            )
            .ok();
        }
        writeln!(out).ok();
    }
}

/// Report blocker (`!`/`!!`) and `::repo` violations detected post-solve.
/// The solver does not model these, so they are surfaced here like slot
/// conflicts rather than failing resolution.
pub(super) fn report_solver_violations(violations: &[portage_atom_pubgrub::Error]) {
    use portage_atom_pubgrub::Error;
    let mut out = anstream::stderr();

    let blockers: Vec<&Error> = violations
        .iter()
        .filter(|e| matches!(e, Error::BlockerConflict { .. }))
        .collect();
    if !blockers.is_empty() {
        writeln!(out, "\n{C_OFF}!!!{C_OFF:#} Blocker conflict(s) detected:\n").ok();
        for e in blockers {
            if let Error::BlockerConflict {
                pkg,
                blocker,
                strength,
            } = e
            {
                writeln!(
                    out,
                    "  {C_PKG}{pkg}{C_PKG:#} blocks {C_OFF}{blocker}{C_OFF:#} ({strength})",
                )
                .ok();
            }
        }
    }

    let repos: Vec<&Error> = violations
        .iter()
        .filter(|e| matches!(e, Error::RepoConstraintConflict(..)))
        .collect();
    if !repos.is_empty() {
        writeln!(
            out,
            "\n{C_OFF}!!!{C_OFF:#} Repository constraint conflict(s) detected:\n"
        )
        .ok();
        for e in repos {
            if let Error::RepoConstraintConflict(pkg, msg) = e {
                writeln!(out, "  {C_PKG}{pkg}{C_PKG:#}: {msg}").ok();
            }
        }
    }
}

/// Report `REQUIRED_USE` constraints left unsatisfied by the planned USE.
/// Mirrors emerge's "following REQUIRED_USE flag constraints are unsatisfied".
pub(super) fn report_required_use(violations: &[super::required_use::RequiredUseViolation]) {
    let mut out = anstream::stderr();
    writeln!(
        out,
        "\n{C_OFF}!!!{C_OFF:#} The following REQUIRED_USE flag constraints are unsatisfied:\n"
    )
    .ok();
    for v in violations {
        writeln!(out, "  {C_PKG}{}-{}{C_PKG:#}", v.cpv.cpn, v.cpv.version).ok();
        for clause in &v.unsatisfied {
            writeln!(out, "    {C_OFF}{clause}{C_OFF:#}").ok();
        }
    }
}

/// Report the USE flags `--autosolve-use` flipped to satisfy `REQUIRED_USE`.
///
/// Flips are grouped onto each in-plan `cpv` (the version the synthetic
/// `package.use` entry keys on), and each block shows the package's
/// `REQUIRED_USE` so the user can see *why* the flag had to move, plus the
/// value their configuration had asked for.
pub(super) fn report_autosolved_use<'a>(
    flips: &[&CededFlag],
    solution: impl IntoIterator<Item = (&'a PortagePackage, &'a Version)>,
    data: &RepoData,
) {
    use std::collections::BTreeMap;

    let mut by_cpn: HashMap<Cpn, Vec<&CededFlag>> = HashMap::new();
    for c in flips {
        by_cpn.entry(c.cpn).or_default().push(c);
    }

    // A flip on a CPN applies to every in-plan version of it (the synthetic
    // package.use above keys per cpv); list each cpv so the report is actionable.
    // BTreeMap keeps the output stable across runs.
    type Block<'a> = (
        Option<&'a portage_metadata::RequiredUseExpr>,
        Vec<&'a CededFlag>,
    );
    let mut blocks: BTreeMap<String, Block> = BTreeMap::new();
    for (pkg, ver) in solution {
        if pkg.is_virtual() {
            continue;
        }
        let Some(pkg_flips) = by_cpn.get(pkg.cpn()) else {
            continue;
        };
        let cpv = format!("{}/{}-{}", pkg.cpn().category, pkg.cpn().package, ver);
        let ru = find_cache(data, pkg, ver).and_then(|c| c.metadata.required_use.as_ref());
        blocks.insert(cpv, (ru, pkg_flips.clone()));
    }
    if blocks.is_empty() {
        return;
    }

    let mut out = anstream::stderr();
    writeln!(
        out,
        "\n{C_PKG}***{C_PKG:#} --autosolve-use adjusted USE flags to satisfy REQUIRED_USE:\n"
    )
    .ok();
    for (cpv, (ru, pkg_flips)) in &blocks {
        writeln!(out, "  {C_PKG}{cpv}{C_PKG:#}").ok();
        for c in pkg_flips {
            let (sign, style) = if c.value { ("+", C_ON) } else { ("-", C_OFF) };
            let configured = if c.value { "off" } else { "on" };
            writeln!(
                out,
                "    {style}{sign}{}{style:#}  {C_OFF}(configured {configured}){C_OFF:#}",
                c.flag.as_str()
            )
            .ok();
        }
        // Show only the REQUIRED_USE clauses that mention a flipped flag — the
        // full constraint can be enormous (e.g. qtbase) and bury the relevant
        // part; deduplicate so two flips sharing a clause print it once.
        if let Some(ru) = ru {
            let mut shown = std::collections::BTreeSet::new();
            for clause in ru.clauses() {
                if pkg_flips.iter().any(|c| clause.mentions(c.flag.as_str()))
                    && shown.insert(clause.to_string())
                {
                    writeln!(out, "    {C_OFF}because:{C_OFF:#} {clause}").ok();
                }
            }
        }
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
            format!(
                ", alternatives: {}",
                alts.iter().cloned().collect::<Vec<_>>().join(" | ")
            )
        };
        eprintln!("note: dropped {pkg_str} ({reason}){alt_str}");
    }
}

/// Format USE flags for display.
///
/// For upgrades/downgrades, if `installed_active_use` is Some, only show flags that differ
/// from the installed version's active USE (emerge -p behavior).
fn format_flags(
    cache: &CacheEntry,
    use_config: &UseConfig,
    use_expand: &[String],
    use_expand_hidden: &[String],
    is_reinstall: bool,
    req: Option<&UseFlagRequirement>,
    installed_active_use: Option<&[Interned<DefaultInterner>]>,
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

        // In diff mode (upgrade/downgrade), skip flags that haven't changed
        if let Some(installed_use) = installed_active_use {
            let installed_enabled = installed_use.contains(&interned);
            // Skip if the enabled state is the same
            if enabled == installed_enabled {
                continue;
            }
        }

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

        if let Some(key) = expand_match {
            let prefix = format!("{}_", key.to_lowercase());
            let short = &name[prefix.len()..];
            let bucket = expand_groups.entry(key.as_str()).or_default();
            if enabled {
                bucket.0.push(format!("{C_ON}{short}{suffix}{C_ON:#}"));
            } else {
                // Wrap disabled USE_EXPAND flags in parentheses
                bucket.1.push(format!("{C_OFF}(-{short}{suffix}){C_OFF:#}"));
            }
        } else if enabled {
            base_flags.0.push(format!("{C_ON}{name}{suffix}{C_ON:#}"));
        } else {
            base_flags
                .1
                .push(format!("{C_OFF}-{name}{suffix}{C_OFF:#}"));
        }
    }

    let join_bucket = |(on, off): &(Vec<String>, Vec<String>)| -> String {
        // Sort enabled and disabled flags separately for portage-compatible ordering
        let mut on_sorted = on.clone();
        let mut off_sorted = off.clone();
        on_sorted.sort();
        off_sorted.sort();
        on_sorted
            .into_iter()
            .chain(off_sorted)
            .collect::<Vec<_>>()
            .join(" ")
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

/// Build the `:slot/subslot::repo` suffix shown in verbose mode.
///
/// Mirrors portage: show `:slot/subslot` when the subslot differs from the
/// slot, else `:slot` when the slot isn't the default `0`, else nothing —
/// always followed by `::repo`.
fn slot_repo_suffix(cache: &CacheEntry, repo_name: &str) -> String {
    let slot = cache.metadata.slot.slot.as_str();
    let subslot = cache.metadata.slot.subslot.map(|s| s.as_str().to_string());
    let mut s = String::new();
    match subslot {
        Some(sub) if sub != slot => s.push_str(&format!(":{slot}/{sub}")),
        _ if slot != "0" => s.push_str(&format!(":{slot}")),
        _ => {}
    }
    s.push_str(&format!("::{repo_name}"));
    s
}

/// Render the emerge-style `Total:` breakdown, e.g.
/// `Total: 26 packages (20 new, 1 upgrade, 5 reinstalls)`.
fn total_line(
    order: &[(PortagePackage, Version)],
    installed: &HashMap<Cpn, HashMap<String, Version>>,
    sizes: &HashMap<Cpv, u64>,
) -> String {
    let (mut new, mut new_slot, mut up, mut down, mut re) = (0, 0, 0, 0, 0);
    for (pkg, ver) in order {
        match action_tag(pkg, ver, installed).0 {
            "N" => new += 1,
            "NS" => new_slot += 1,
            "U" => up += 1,
            "D" => down += 1,
            "R" => re += 1,
            _ => {}
        }
    }
    let plural = |n: usize, s: &str| format!("{n} {s}{}", if n == 1 { "" } else { "s" });
    // Order mirrors portage's PackageCounters.__str__: upgrades, downgrades,
    // new, in new slot, reinstall.
    let mut parts = Vec::new();
    if up > 0 {
        parts.push(plural(up, "upgrade"));
    }
    if down > 0 {
        parts.push(plural(down, "downgrade"));
    }
    if new > 0 {
        parts.push(format!("{new} new"));
    }
    if new_slot > 0 {
        parts.push(plural(new_slot, "in new slot"));
    }
    if re > 0 {
        parts.push(plural(re, "reinstall"));
    }

    let n = order.len();
    let pkgs = if n == 1 { "package" } else { "packages" };
    let total_bytes: u64 = order
        .iter()
        .map(|(pkg, ver)| {
            sizes
                .get(&Cpv::new(*pkg.cpn(), ver.clone()))
                .copied()
                .unwrap_or(0)
        })
        .sum();
    let downloads = format!(", Size of downloads: {}", format_kib(total_bytes));
    if parts.is_empty() {
        format!("\nTotal: {n} {pkgs}{downloads}")
    } else {
        format!("\nTotal: {n} {pkgs} ({}){downloads}", parts.join(", "))
    }
}

/// Build the 7-char status field that follows `[ebuild ` in the merge list,
/// placing each action letter at the fixed column portage uses so columns line
/// up across rows: `N`/`NS` (new / new slot), `R` (reinstall), `U`/`D`
/// (upgrade / downgrade).
fn status_field(tag: &str) -> String {
    let mut f = [b' '; 7];
    match tag {
        "N" => f[1] = b'N',
        "NS" => {
            f[1] = b'N';
            f[2] = b'S';
        }
        "R" => f[2] = b'R',
        "U" => f[4] = b'U',
        "D" => f[5] = b'D',
        "UD" => {
            f[4] = b'U';
            f[5] = b'D';
        }
        _ => {}
    }
    String::from_utf8(f.to_vec()).unwrap()
}

/// Colorize the status field characters according to portage conventions:
/// - N: green
/// - S: green  
/// - U: cyan
/// - D: blue
/// - R: yellow
fn colorize_status_field(field: &str) -> String {
    let mut result = String::new();
    for (i, c) in field.chars().enumerate() {
        let style = match (i, c) {
            (1, 'N') => C_STATUS_N,
            (2, 'S') => C_STATUS_S,
            (2, 'R') => C_STATUS_R,
            (4, 'U') => C_STATUS_U,
            (5, 'D') => C_STATUS_D,
            _ => Style::new(), // No color for spaces or other positions
        };
        result.push_str(&format!("{style}{c}{style:#}"));
    }
    result
}

/// Format a byte count as emerge does: ceil-divided to KiB (e.g. `569527` →
/// `557 KiB`, `0` → `0 KiB`). emerge's thousands grouping is locale-dependent
/// and absent under the C locale, so none is applied here.
fn format_kib(bytes: u64) -> String {
    format!("{} KiB", bytes.div_ceil(1024))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn print_pretty(
    data: &RepoData,
    order: &[(PortagePackage, Version)],
    installed: &HashMap<Cpn, HashMap<String, Version>>,
    installed_entries: &[super::installed::VdbEntry],
    use_config: &UseConfig,
    package_use: &[(Dep, Vec<String>)],
    use_expand: &[String],
    use_expand_hidden: &[String],
    flag_reqs: &HashMap<&PortagePackage, &UseFlagRequirement>,
    sizes: &HashMap<Cpv, u64>,
    verbose: bool,
) {
    let mut out = anstream::stdout();

    writeln!(
        out,
        "{C_PKG}These are the packages that would be merged, in order:{C_PKG:#}\n"
    )
    .ok();
    writeln!(out, "Calculating dependencies... done!").ok();

    for (pkg, ver) in order {
        let cpn = pkg.cpn();
        let (tag, old_ver) = action_tag(pkg, ver, installed);
        let req = flag_reqs.get(pkg).copied();
        let is_reinstall = tag == "R";
        let cache = find_cache(data, pkg, ver);

        // Verbose mode shows USE/expand flags and the slot/subslot::repo suffix;
        // plain mode mirrors `emerge -p` and lists just the versioned atom.
        let (flag_str, slot_repo) = if verbose {
            let cpv = Cpv::new(*cpn, ver.clone());
            let effective_use = apply_package_use(use_config, &cpv, pkg.slot(), package_use);

            // For upgrades/downgrades, find the installed entry to compare USE flags
            let installed_active_use = if tag == "U" || tag == "D" {
                let slot_key = pkg
                    .slot()
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_default();
                installed_entries
                    .iter()
                    .find(|e| e.cpn == *cpn && e.slot.as_deref() == Some(slot_key.as_str()))
                    .map(|e| e.active_use.as_slice())
            } else {
                None
            };

            let flags = cache
                .map(|c| {
                    format_flags(
                        c,
                        &effective_use,
                        use_expand,
                        use_expand_hidden,
                        is_reinstall,
                        req,
                        installed_active_use,
                    )
                })
                .unwrap_or_default();
            let suffix = cache
                .map(|c| slot_repo_suffix(c, &data.repo_name))
                .unwrap_or_default();
            (flags, suffix)
        } else {
            (String::new(), String::new())
        };

        // emerge shows the previously-installed version only for upgrades and
        // downgrades, not for same-version reinstalls or new installs.
        let old = match tag {
            "U" | "D" => old_ver.map(|v| format!(" [{}]", v)).unwrap_or_default(),
            _ => String::new(),
        };
        // Verbose mode appends the download size (distfiles not in DISTDIR).
        let size_str = if verbose {
            let cpv = Cpv::new(*cpn, ver.clone());
            format!(" {}", format_kib(sizes.get(&cpv).copied().unwrap_or(0)))
        } else {
            String::new()
        };
        let field = status_field(tag);
        let colored_field = colorize_status_field(&field);
        writeln!(
            out,
            "[{C_BRACKET}ebuild {colored_field}{C_BRACKET:#}] {C_PKG}{cpn}-{ver}{slot_repo}{C_PKG:#}{old}{flag_str}{size_str}",
        ).ok();
    }

    // emerge only prints the Total line in verbose mode.
    if verbose {
        writeln!(out, "{}", total_line(order, installed, sizes)).ok();
    }
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

pub(super) const C_DIM: Style = Style::new().effects(Effects::DIMMED);

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
    // deduplicate children (same package may appear via multiple dep classes,
    // and not necessarily adjacently — DEPEND/BDEPEND edges to the same package
    // can be interleaved with others, so a positional dedup is insufficient).
    for kids in children.values_mut() {
        let mut seen: std::collections::HashSet<&PortagePackage> = Default::default();
        kids.retain(|(pkg, _)| seen.insert(*pkg));
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
            if already {
                format!(" {C_DIM}(*){C_DIM:#}")
            } else {
                String::new()
            }
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
