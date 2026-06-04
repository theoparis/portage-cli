use std::collections::HashMap;
use std::io::Write as _;

use camino::Utf8Path;
use portage_atom::Version;
use portage_atom_pubgrub::UseFlagRequirement;

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
) -> Vec<PackageUseEntry> {
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
        let atom = format!(">={}-{}", cpn, ver_str(ver));

        let mut flags: Vec<String> = Vec::new();
        for f in &req.required_enabled {
            flags.push(f.as_str().to_string());
        }
        for f in &req.required_disabled {
            flags.push(format!("-{}", f.as_str()));
        }

        let comments = build_comments(req, root_atoms);

        by_file
            .entry(filename)
            .or_default()
            .push(PackageUseLine { comments, atom, flags });
    }

    by_file
        .into_iter()
        .map(|(filename, lines)| PackageUseEntry { filename, lines })
        .collect()
}

fn ver_str(v: &Version) -> String {
    v.to_string()
}

fn build_comments(req: &UseFlagRequirement, root_atoms: &[String]) -> Vec<String> {
    let mut comments = Vec::new();

    // Direct requirers from the solver (most specific).
    if !req.required_by.is_empty() {
        let list = req.required_by.join(", ");
        comments.push(format!("# required by {list}"));
    } else {
        // Fall back to root atoms.
        let list = root_atoms.join(", ");
        comments.push(format!("# required by {list} (argument)"));
    }

    comments
}

/// Print the required USE changes to stderr in portage style.
pub(super) fn report(entries: &[PackageUseEntry]) {
    if entries.is_empty() {
        return;
    }
    let mut out = anstream::stderr();
    writeln!(out, "\nThe following USE changes are necessary to proceed:").ok();
    writeln!(out, " (see \"package.use\" in the portage(5) man page for more details)").ok();
    for entry in entries {
        for line in &entry.lines {
            for comment in &line.comments {
                writeln!(out, "{comment}").ok();
            }
            writeln!(out, "{} {}", line.atom, line.flags.join(" ")).ok();
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
            std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {path}"))?
        } else {
            String::new()
        };

        // Build the new content: keep existing lines, append new atoms that
        // aren't already present, update atoms whose flags have changed.
        let new_content = merge_content(&existing, &entry.lines);
        std::fs::write(&path, &new_content)
            .with_context(|| format!("failed to write {path}"))?;
        eprintln!("Written: {path}");
    }
    Ok(())
}

/// Merge new lines into existing file content.
///
/// Atoms already present in the file are updated in-place; new ones are
/// appended.  Existing lines unrelated to the new entries are preserved.
fn merge_content(existing: &str, lines: &[PackageUseLine]) -> String {
    let mut output: Vec<String> = existing
        .lines()
        .map(|l| l.to_string())
        .collect();

    // Remove trailing blank lines so we append cleanly.
    while output.last().map(|l: &String| l.trim().is_empty()).unwrap_or(false) {
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
            // Replace the existing entry.
            output[pos] = new_line;
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
