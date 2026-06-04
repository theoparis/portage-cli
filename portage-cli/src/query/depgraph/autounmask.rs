use std::io::Write as _;

use camino::Utf8Path;

use super::repo::AutounmaskCandidate;
use super::repo::FilterReason;

/// One line to write into a portage config file.
struct Entry {
    filename: String,
    atom: String,
    /// Trailing tokens after the atom (keywords, licenses); empty for unmask.
    tokens: Vec<String>,
}

fn filename(cpv: &portage_atom::Cpv) -> String {
    format!(
        "{}-{}",
        cpv.cpn.category.as_str().replace('/', "-"),
        cpv.cpn.package.as_str()
    )
}

fn atom(cpv: &portage_atom::Cpv, slot: Option<portage_atom::interner::Interned<portage_atom::interner::DefaultInterner>>) -> String {
    let slot_suffix = slot
        .map(|s| format!(":{}", s.as_str()))
        .unwrap_or_default();
    format!("={}-{}{}", cpv.cpn, cpv.version, slot_suffix)
}

fn build_entries(
    candidates: &[AutounmaskCandidate],
    kind: &str,
) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for c in candidates {
        let a = atom(&c.cpv, c.slot);
        let f = filename(&c.cpv);
        for reason in &c.reasons {
            match (kind, reason) {
                ("keywords", FilterReason::Keyword(kw)) => {
                    entries.push(Entry { filename: f.clone(), atom: a.clone(), tokens: vec![kw.clone()] });
                }
                ("unmask", FilterReason::Masked) => {
                    entries.push(Entry { filename: f.clone(), atom: a.clone(), tokens: vec![] });
                }
                ("license", FilterReason::License(lics)) => {
                    entries.push(Entry { filename: f.clone(), atom: a.clone(), tokens: lics.clone() });
                }
                _ => {}
            }
        }
    }
    // Deduplicate by (filename, atom, tokens).
    entries.sort_by(|a, b| (&a.filename, &a.atom).cmp(&(&b.filename, &b.atom)));
    entries.dedup_by(|a, b| a.filename == b.filename && a.atom == b.atom && a.tokens == b.tokens);
    entries
}

fn format_line(e: &Entry) -> String {
    if e.tokens.is_empty() {
        e.atom.clone()
    } else {
        format!("{} {}", e.atom, e.tokens.join(" "))
    }
}

/// Print keyword changes to stderr in portage style.
pub(super) fn report(candidates: &[AutounmaskCandidate]) {
    let kw = build_entries(candidates, "keywords");
    let unmask = build_entries(candidates, "unmask");
    let lic = build_entries(candidates, "license");

    if kw.is_empty() && unmask.is_empty() && lic.is_empty() {
        return;
    }

    let mut out = anstream::stderr();
    if !kw.is_empty() {
        writeln!(out, "\nThe following keyword changes are necessary to proceed:").ok();
        writeln!(out, " (see \"package.accept_keywords\" in the portage(5) man page for more details)").ok();
        for e in &kw {
            writeln!(out, "{}", format_line(e)).ok();
        }
    }
    if !unmask.is_empty() {
        writeln!(out, "\nThe following mask changes are necessary to proceed:").ok();
        writeln!(out, " (see \"package.unmask\" in the portage(5) man page for more details)").ok();
        for e in &unmask {
            writeln!(out, "{}", format_line(e)).ok();
        }
    }
    if !lic.is_empty() {
        writeln!(out, "\nThe following license changes are necessary to proceed:").ok();
        writeln!(out, " (see \"package.license\" in the portage(5) man page for more details)").ok();
        for e in &lic {
            writeln!(out, "{}", format_line(e)).ok();
        }
    }
}

/// Write autounmask entries to the appropriate files under `portage_dir`
/// (`portage_dir` is e.g. `/etc/portage`).
pub(super) fn write(candidates: &[AutounmaskCandidate], portage_dir: &Utf8Path) -> anyhow::Result<()> {
    write_kind(candidates, "keywords", &portage_dir.join("package.accept_keywords"))?;
    write_kind(candidates, "unmask",   &portage_dir.join("package.unmask"))?;
    write_kind(candidates, "license",  &portage_dir.join("package.license"))?;
    Ok(())
}

fn write_kind(candidates: &[AutounmaskCandidate], kind: &str, dir: &Utf8Path) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let entries = build_entries(candidates, kind);
    if entries.is_empty() {
        return Ok(());
    }

    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create {dir}"))?;

    // Group by filename.
    let mut by_file: std::collections::HashMap<&str, Vec<&Entry>> = std::collections::HashMap::new();
    for e in &entries {
        by_file.entry(e.filename.as_str()).or_default().push(e);
    }

    for (filename, lines) in by_file {
        let path = dir.join(filename);
        let existing = if path.exists() {
            std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {path}"))?
        } else {
            String::new()
        };
        let new_content = merge_content(&existing, lines);
        std::fs::write(&path, &new_content)
            .with_context(|| format!("failed to write {path}"))?;
        eprintln!("Written: {path}");
    }
    Ok(())
}

fn merge_content(existing: &str, lines: Vec<&Entry>) -> String {
    let mut output: Vec<String> = existing.lines().map(str::to_string).collect();
    while output.last().map(|l: &String| l.trim().is_empty()).unwrap_or(false) {
        output.pop();
    }
    for entry in lines {
        let new_line = format_line(entry);
        let existing_pos = output.iter().position(|l| {
            l.split_whitespace().next() == Some(entry.atom.as_str())
        });
        if let Some(pos) = existing_pos {
            output[pos] = new_line;
        } else {
            if !output.is_empty() {
                output.push(String::new());
            }
            output.push(new_line);
        }
    }
    let mut result = output.join("\n");
    result.push('\n');
    result
}
