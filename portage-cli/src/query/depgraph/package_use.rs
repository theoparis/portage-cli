use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write as _;

use camino::Utf8Path;
use portage_atom::{Cpv, Dep, Version};
use portage_atom_pubgrub::{
    DepEdge, UseConfig, UseFlagRequirement, UseFlagState, apply_package_use,
};

/// Entries to write into `/etc/portage/package.use`.
pub(super) struct PackageUseEntry {
    /// Filename inside `package.use/`: category-package (e.g. `dev-python-pygments`).
    pub filename: String,
    /// Lines to add/update in that file.
    pub lines: Vec<PackageUseLine>,
}

pub(super) struct PackageUseLine {
    /// Comment lines explaining the requirement, e.g. `# required by firefox`.
    pub comments: Vec<String>,
    /// The atom spec, e.g. `>=dev-python/pygments-2.19.2`.
    pub atom: String,
    /// Flags to enable (no prefix) and disable (`-` prefix).
    pub flags: Vec<String>,
}

/// Build `package.use` entries for all non-trivial USE flag requirements.
pub(super) fn build_entries(
    flag_reqs: &[UseFlagRequirement],
    root_atoms: &[String],
    edges: &[DepEdge],
    use_config: &UseConfig,
    package_use: &[(Dep, Vec<String>)],
) -> Vec<PackageUseEntry> {
    // Pre-compute once for all requirements.
    let adj = build_adjacency(edges);
    let root_cpns = parse_root_cpns(root_atoms);

    let mut by_file: HashMap<String, Vec<PackageUseLine>> = HashMap::new();

    for req in flag_reqs {
        if req.required_enabled.is_empty() && req.required_disabled.is_empty() {
            continue;
        }
        if req.package.is_virtual() {
            continue;
        }

        let cpn = req.package.cpn();
        let filename = format!(
            "{}-{}",
            cpn.category.as_str().replace('/', "-"),
            cpn.package.as_str()
        );

        let ver = req.upgrade_to.as_ref().unwrap_or(&req.version);
        let slot_suffix = req
            .package
            .slot()
            .map(|s| format!(":{}", s.as_str()))
            .unwrap_or_default();
        let atom = format!(">={}-{}{}", cpn, ver_str(ver), slot_suffix);

        // A flag belongs in package.use only if the global config does not
        // already set it the way the requirement needs.  A flag already enabled
        // (e.g. a PYTHON_TARGETS member in the profile) just triggers a rebuild,
        // shown via the `*` USE marker — it is not an autounmask change.
        let cpv = Cpv::new(*cpn, ver.clone());
        let eff = apply_package_use(use_config, &cpv, req.package.slot(), package_use);
        let mut flags: Vec<String> = Vec::new();
        for f in &req.required_enabled {
            if eff.get_opt(f) != Some(UseFlagState::Enabled) {
                flags.push(f.as_str().to_string());
            }
        }
        for f in &req.required_disabled {
            if eff.get_opt(f) != Some(UseFlagState::Disabled) {
                flags.push(format!("-{}", f.as_str()));
            }
        }
        if flags.is_empty() {
            continue;
        }

        let comments = build_comments(req, root_atoms, &root_cpns, &adj);

        by_file.entry(filename).or_default().push(PackageUseLine {
            comments,
            atom,
            flags,
        });
    }

    // by_file is a HashMap; sort by filename so the report/written output is
    // reproducible across runs.  Lines within a file follow flag_reqs order,
    // which is already deterministic (use_flag_requirements is sorted).
    let mut entries: Vec<PackageUseEntry> = by_file
        .into_iter()
        .map(|(filename, lines)| PackageUseEntry { filename, lines })
        .collect();
    entries.sort_by(|a, b| a.filename.cmp(&b.filename));
    entries
}

fn ver_str(v: &Version) -> String {
    v.to_string()
}

/// C7: auto-apply cross-package `[flag]` USE-deps to a fixpoint.
///
/// Starting from `package_use`, repeatedly: solve (`solve` returns the in-plan
/// USE-flag requirements, or `None` if the solve failed), force every demanded
/// flag that is real IUSE of its target via a synthetic `cpn flags` entry, and
/// re-solve — until no new flag is added. Returns the augmented `package_use`.
///
/// `applied` (a flag forced once) is never re-forced, so a `[bar]` vs `[-bar]`
/// contradiction resolves to first-wins + advisory for the loser rather than
/// oscillating; the bound is a backstop. Caller gates this on `--autosolve-use`.
pub(super) fn cosolve_use_deps<F>(
    mut package_use: Vec<(Dep, Vec<String>)>,
    data: &super::repo::RepoData,
    solve: F,
) -> Vec<(Dep, Vec<String>)>
where
    F: Fn(&[(Dep, Vec<String>)]) -> Option<Vec<UseFlagRequirement>>,
{
    use portage_atom::Cpn;
    use portage_atom::interner::{DefaultInterner, Interned};

    let mut applied: HashMap<(Cpn, Interned<DefaultInterner>), bool> = HashMap::new();
    for _ in 0..8 {
        let Some(reqs) = solve(&package_use) else {
            break;
        };
        let mut new_by_cpn: HashMap<Cpn, Vec<String>> = HashMap::new();
        for (cpn, flag, enable) in solver_use_dep_targets(&reqs, data) {
            if applied.contains_key(&(cpn, flag)) {
                continue; // already forced (same or opposite) — don't oscillate
            }
            applied.insert((cpn, flag), enable);
            let tok = if enable {
                flag.as_str().to_string()
            } else {
                format!("-{}", flag.as_str())
            };
            new_by_cpn.entry(cpn).or_default().push(tok);
        }
        if new_by_cpn.is_empty() {
            break;
        }
        for (cpn, flags) in new_by_cpn {
            if let Ok(dep) = Dep::parse(&cpn.to_string()) {
                package_use.push((dep, flags));
            }
        }
    }
    package_use
}

/// The `(target cpn, flag, enable)` triples that in-plan cross-package `[flag]`
/// deps demand, filtered to flags that are **real IUSE** of the target's
/// selected/upgrade version (a `[bar]` on a target without `bar` cannot be
/// applied — CC7). Used by C7 (`--autosolve-use`) to auto-apply USE-deps via
/// synthetic `package.use` and re-solve, instead of only suggesting them.
pub(super) fn solver_use_dep_targets(
    flag_reqs: &[UseFlagRequirement],
    data: &super::repo::RepoData,
) -> Vec<(
    portage_atom::Cpn,
    portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>,
    bool,
)> {
    let mut out = Vec::new();
    for req in flag_reqs {
        if req.package.is_virtual()
            || (req.required_enabled.is_empty() && req.required_disabled.is_empty())
        {
            continue;
        }
        let cpn = *req.package.cpn();
        let ver = req.upgrade_to.as_ref().unwrap_or(&req.version);
        let iuse: HashSet<String> = super::repo::find_cache(data, &req.package, ver)
            .map(|c| {
                c.metadata
                    .iuse
                    .iter()
                    .map(|i| i.name().to_string())
                    .collect()
            })
            .unwrap_or_default();
        for f in &req.required_enabled {
            if iuse.contains(f.as_str()) {
                out.push((cpn, *f, true));
            }
        }
        for f in &req.required_disabled {
            if iuse.contains(f.as_str()) {
                out.push((cpn, *f, false));
            }
        }
    }
    out
}

/// Adjacency map: CPN → Vec<(to_CPN, annotation)>.
/// annotation = "from-cpv[flag]" when gated, "from-cpv" otherwise.
type Adjacency = HashMap<String, Vec<(String, String)>>;

fn build_adjacency(edges: &[DepEdge]) -> Adjacency {
    let mut adj: Adjacency = HashMap::new();
    for e in edges {
        if e.from.0.is_virtual() || e.to.0.is_virtual() {
            continue;
        }
        let from_cpn = e.from.0.cpn().to_string();
        let from_cpv = format!("{}-{}", e.from.0.cpn(), e.from.1);
        let annotation = match e.via_use_flag {
            Some(f) => format!("{}[{}]", from_cpv, f.as_str()),
            None => from_cpv,
        };
        let to_cpn = e.to.0.cpn().to_string();
        adj.entry(from_cpn).or_default().push((to_cpn, annotation));
    }
    adj
}

/// Strip operators and version suffix from a root atom to get "cat/pkg".
fn parse_root_cpns(root_atoms: &[String]) -> HashSet<String> {
    root_atoms
        .iter()
        .map(|r| {
            let base = r.trim_start_matches(['>', '<', '=', '~', '!']);
            if let Some(slash) = base.find('/') {
                let after_slash = &base[slash + 1..];
                if let Some(rel) = after_slash.rfind(|c: char| c == '-').and_then(|i| {
                    after_slash[i + 1..]
                        .chars()
                        .next()
                        .filter(char::is_ascii_digit)
                        .map(|_| i)
                }) {
                    return format!("{}/{}", &base[..slash], &after_slash[..rel]);
                }
            }
            base.to_string()
        })
        .collect()
}

fn build_comments(
    req: &UseFlagRequirement,
    root_atoms: &[String],
    root_cpns: &HashSet<String>,
    adj: &Adjacency,
) -> Vec<String> {
    let target_key = req.package.cpn().to_string();

    // BFS: (current_CPN, path_of_annotations_so_far)
    // path grows as we walk from a root toward the target.
    let mut queue: VecDeque<(String, Vec<String>)> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();

    // Seed with edges whose source is exactly a root CPN.
    for (from_cpn, neighbors) in adj {
        if root_cpns.contains(from_cpn) {
            for (to_cpn, annotation) in neighbors {
                queue.push_back((to_cpn.clone(), vec![annotation.clone()]));
            }
            visited.insert(from_cpn.clone());
        }
    }

    let mut found_path: Option<Vec<String>> = None;
    'bfs: while let Some((current, path)) = queue.pop_front() {
        if current == target_key {
            found_path = Some(path);
            break 'bfs;
        }
        if !visited.insert(current.clone()) {
            continue;
        }
        if let Some(neighbors) = adj.get(&current) {
            for (to_cpn, annotation) in neighbors {
                if !visited.contains(to_cpn) {
                    let mut new_path = path.clone();
                    new_path.push(annotation.clone());
                    queue.push_back((to_cpn.clone(), new_path));
                }
            }
        }
    }

    let mut comments = Vec::new();
    if let Some(path) = found_path {
        // Show chain from deepest (closest to target) back to root.
        for hop in path.iter().rev() {
            comments.push(format!("# required by {hop}"));
        }
        let roots = root_atoms.join(", ");
        comments.push(format!("# required by {roots} (argument)"));
    } else if !req.required_by.is_empty() {
        // Fallback: solver-level immediate requirers.
        for r in &req.required_by {
            comments.push(format!("# required by {r}"));
        }
    } else {
        let list = root_atoms.join(", ");
        comments.push(format!("# required by {list} (argument)"));
    }
    comments
}

/// Print the required USE changes to stderr in portage style.
pub(super) fn report(entries: &[PackageUseEntry]) {
    use super::output::{C_DIM, C_OFF, C_ON, C_PKG};

    if entries.is_empty() {
        return;
    }
    let mut out = anstream::stderr();
    writeln!(
        out,
        "\n{C_PKG}The following USE changes are necessary to proceed:{C_PKG:#}"
    )
    .ok();
    writeln!(
        out,
        " (see \"package.use\" in the portage(5) man page for more details)"
    )
    .ok();
    for entry in entries {
        for line in &entry.lines {
            for comment in &line.comments {
                writeln!(out, "{C_DIM}{comment}{C_DIM:#}").ok();
            }
            let flag_str: String = line
                .flags
                .iter()
                .map(|f| {
                    if f.starts_with('-') {
                        format!("{C_OFF}{f}{C_OFF:#}")
                    } else {
                        format!("{C_ON}{f}{C_ON:#}")
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            writeln!(out, "{C_PKG}{}{C_PKG:#} {flag_str}", line.atom).ok();
        }
    }
}

/// Write entries to `/etc/portage/package.use/{filename}`, creating/updating
/// the file and inserting a block comment pointing to the requesting version.
pub(super) fn write(entries: &[PackageUseEntry], package_use_dir: &Utf8Path) -> anyhow::Result<()> {
    use anyhow::Context as _;
    std::fs::create_dir_all(package_use_dir)
        .with_context(|| format!("failed to create {package_use_dir}"))?;

    for entry in entries {
        let path = package_use_dir.join(&entry.filename);
        let existing = if path.exists() {
            std::fs::read_to_string(&path).with_context(|| format!("failed to read {path}"))?
        } else {
            String::new()
        };

        // Build the new content: keep existing lines, append new atoms that
        // aren't already present, update atoms whose flags have changed.
        let new_content = merge_content(&existing, &entry.lines);
        std::fs::write(&path, &new_content).with_context(|| format!("failed to write {path}"))?;
        eprintln!("Written: {path}");
    }
    Ok(())
}

/// Merge new lines into existing file content.
///
/// Atoms already present in the file are updated in-place (flags and comments
/// both replaced); new ones are appended.  Existing lines unrelated to the
/// new entries are preserved.
fn merge_content(existing: &str, lines: &[PackageUseLine]) -> String {
    let mut output: Vec<String> = existing.lines().map(|l| l.to_string()).collect();

    // Remove trailing blank lines so we append cleanly.
    while output
        .last()
        .map(|l: &String| l.trim().is_empty())
        .unwrap_or(false)
    {
        output.pop();
    }

    for line in lines {
        // Check if a line for this atom already exists.
        let existing_pos = output.iter().position(|l| {
            let tok: Vec<&str> = l.split_whitespace().collect();
            tok.first() == Some(&line.atom.as_str())
        });

        let new_line = format!("{} {}", line.atom, line.flags.join(" "));

        if let Some(pos) = existing_pos {
            // Scan backwards to find the start of the comment block above
            // this atom line, so we can replace it along with the atom.
            let mut comment_start = pos;
            while comment_start > 0 && output[comment_start - 1].trim_start().starts_with('#') {
                comment_start -= 1;
            }
            let new_block: Vec<String> = line
                .comments
                .iter()
                .cloned()
                .chain(std::iter::once(new_line))
                .collect();
            output.splice(comment_start..=pos, new_block);
        } else {
            // Append with comment header.
            if !output.is_empty() {
                output.push(String::new());
            }
            for comment in &line.comments {
                output.push(comment.clone());
            }
            output.push(new_line);
        }
    }

    let mut result = output.join("\n");
    result.push('\n');
    result
}
