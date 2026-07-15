//! Minimal single-level INI parsing shared by `repos.conf`/`binrepos.conf`-style
//! config files: `[section]` headers, `key = value` lines, `#`/`;` comments,
//! later files overriding earlier keys within the same section. No value
//! interpolation (`%(VAR)s`) — real portage's `ConfigParser` supports it, but
//! no configured value either format actually needs it here, so this is a
//! documented simplification rather than a silent gap.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::repo::util;

/// Resolve `path` to the `.conf` files it contributes, in application order:
/// a single file as itself, or (if a directory) every `*.conf` file within,
/// sorted by name. Missing paths are silently skipped (empty result) —
/// callers treat an absent config file/dir as "nothing configured here".
pub fn collect_conf_files(path: &Path) -> Result<Vec<PathBuf>> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(util::io_err(path, e)),
    };
    if meta.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !meta.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(path)
        .map_err(|e| util::io_err(path, e))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("conf"))
        .collect();
    files.sort();
    Ok(files)
}

/// Parse one file's contents into `sections` (accumulated across files —
/// later files override earlier keys within the same section) and `order`
/// (first-seen section order, `[DEFAULT]` excluded — callers that need
/// `DEFAULT` read it from `sections` directly).
pub fn merge_sections(
    sections: &mut HashMap<String, HashMap<String, String>>,
    order: &mut Vec<String>,
    contents: &str,
) {
    let mut current: String = "DEFAULT".to_string();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(stripped) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = stripped.trim().to_string();
            if !sections.contains_key(&current) {
                if current != "DEFAULT" {
                    order.push(current.clone());
                }
                sections.insert(current.clone(), HashMap::new());
            }
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            sections
                .entry(current.clone())
                .or_default()
                .insert(k.trim().to_string(), v.trim().to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_sections_tracks_first_seen_order_and_overrides() {
        let mut sections = HashMap::new();
        let mut order = Vec::new();
        merge_sections(&mut sections, &mut order, "[a]\nx = 1\n[b]\ny = 2\n");
        merge_sections(&mut sections, &mut order, "[a]\nx = 3\n");
        assert_eq!(order, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(sections["a"]["x"], "3");
        assert_eq!(sections["b"]["y"], "2");
    }

    #[test]
    fn merge_sections_skips_comments_and_blank_lines() {
        let mut sections = HashMap::new();
        let mut order = Vec::new();
        merge_sections(
            &mut sections,
            &mut order,
            "# comment\n\n[a]\n; also a comment\nx = 1\n",
        );
        assert_eq!(sections["a"]["x"], "1");
    }
}
