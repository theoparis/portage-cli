use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::repository::Repository;
use super::util;
use crate::error::Result;

/// A single repository entry parsed from `repos.conf`.
#[derive(Debug, Clone)]
pub struct RepoEntry {
    /// Section name (e.g. `gentoo`, `crossdev`).
    pub name: String,
    /// Absolute path to the repository root.
    pub location: PathBuf,
    /// Names of master repositories (often empty; layout.conf normally wins).
    pub masters: Vec<String>,
}

/// Parsed `repos.conf` describing every configured repository.
///
/// The Gentoo `repos.conf` format is read from multiple locations in
/// override order. Sections sharing a `[name]` are merged key-by-key,
/// with later files overriding earlier ones. The `[DEFAULT]` section's
/// `main-repo` key selects which repo is the main one (placed first).
///
/// See [Repository format — repos.conf](https://wiki.gentoo.org/wiki/Handbook:AMD64/Portage/CustomTree#Defining_a_custom_repository).
#[derive(Debug, Clone, Default)]
pub struct ReposConf {
    repos: Vec<RepoEntry>,
    main_repo: Option<String>,
}

impl ReposConf {
    /// Load `repos.conf` using portage's default search paths:
    /// `/usr/share/portage/config/repos.conf` (defaults), then
    /// `/etc/portage/repos.conf` (file or directory of `*.conf`).
    pub fn load() -> Result<Self> {
        Self::load_from(&[
            Path::new("/usr/share/portage/config/repos.conf"),
            Path::new("/etc/portage/repos.conf"),
        ])
    }

    /// Load from explicit paths in override order. Each path may be a file
    /// or a directory; directories contribute every `*.conf` they contain
    /// in alphabetical order. Missing paths are silently skipped.
    pub fn load_from<P: AsRef<Path>>(paths: &[P]) -> Result<Self> {
        let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut order: Vec<String> = Vec::new();

        for path in paths {
            for file in collect_conf_files(path.as_ref())? {
                let contents = match std::fs::read_to_string(&file) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(util::io_err(&file, e)),
                };
                merge_into(&mut sections, &mut order, &contents);
            }
        }

        let main_repo = sections
            .get("DEFAULT")
            .and_then(|s| s.get("main-repo"))
            .cloned();

        let mut repos: Vec<RepoEntry> = order
            .iter()
            .filter_map(|name| {
                let s = sections.get(name)?;
                let location = s.get("location")?;
                let masters = s
                    .get("masters")
                    .map(|v| v.split_whitespace().map(String::from).collect())
                    .unwrap_or_default();
                Some(RepoEntry {
                    name: name.clone(),
                    location: PathBuf::from(location),
                    masters,
                })
            })
            .collect();

        if let Some(main) = main_repo.as_deref()
            && let Some(pos) = repos.iter().position(|r| r.name == main)
            && pos != 0
        {
            let m = repos.remove(pos);
            repos.insert(0, m);
        }

        Ok(ReposConf { repos, main_repo })
    }

    /// Every configured repository in resolution order (main first).
    pub fn repos(&self) -> &[RepoEntry] {
        &self.repos
    }

    /// The main repo, if a `[DEFAULT] main-repo` is set and resolves.
    pub fn main_repo(&self) -> Option<&RepoEntry> {
        let name = self.main_repo.as_deref()?;
        self.repos.iter().find(|r| r.name == name)
    }

    /// Look up an entry by repository name.
    pub fn find(&self, name: &str) -> Option<&RepoEntry> {
        self.repos.iter().find(|r| r.name == name)
    }

    /// Open every configured repository. Main repo first; rest in
    /// configuration order. Fails on the first `Repository::open` error.
    pub fn open_all(&self) -> Result<Vec<Repository>> {
        self.repos
            .iter()
            .map(|e| Repository::open(&e.location))
            .collect()
    }
}

fn collect_conf_files(path: &Path) -> Result<Vec<PathBuf>> {
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

fn merge_into(
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
    use std::io::Write;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn parse_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("repos.conf");
        write(
            &conf,
            r#"
[DEFAULT]
main-repo = gentoo

[gentoo]
location = /var/db/repos/gentoo

[crossdev]
location = /var/db/repos/crossdev
masters = gentoo
"#,
        );
        let rc = ReposConf::load_from(&[&conf]).unwrap();
        assert_eq!(rc.repos().len(), 2);
        assert_eq!(rc.repos()[0].name, "gentoo");
        assert_eq!(rc.repos()[1].name, "crossdev");
        assert_eq!(rc.repos()[1].masters, vec!["gentoo"]);
        assert_eq!(rc.main_repo().map(|r| r.name.as_str()), Some("gentoo"));
    }

    #[test]
    fn merges_directory_alphabetical() {
        let dir = tempfile::tempdir().unwrap();
        let confdir = dir.path().join("repos.conf");
        write(
            &confdir.join("00-defaults.conf"),
            "[DEFAULT]\nmain-repo = gentoo\n[gentoo]\nlocation = /a\n",
        );
        write(
            &confdir.join("10-overlay.conf"),
            "[overlay]\nlocation = /b\n",
        );
        let rc = ReposConf::load_from(&[&confdir]).unwrap();
        let names: Vec<_> = rc.repos().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["gentoo", "overlay"]);
    }

    #[test]
    fn later_path_overrides_earlier() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.conf");
        let b = dir.path().join("b.conf");
        write(&a, "[gentoo]\nlocation = /old\n");
        write(&b, "[gentoo]\nlocation = /new\n");
        let rc = ReposConf::load_from(&[&a, &b]).unwrap();
        assert_eq!(rc.find("gentoo").unwrap().location, PathBuf::from("/new"));
    }

    #[test]
    fn missing_paths_are_silently_skipped() {
        let rc = ReposConf::load_from(&[Path::new("/nonexistent/path")]).unwrap();
        assert!(rc.repos().is_empty());
    }

    #[test]
    fn main_repo_moves_to_front_even_when_declared_later() {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("repos.conf");
        write(
            &conf,
            r#"
[overlay]
location = /b

[gentoo]
location = /a

[DEFAULT]
main-repo = gentoo
"#,
        );
        let rc = ReposConf::load_from(&[&conf]).unwrap();
        assert_eq!(rc.repos()[0].name, "gentoo");
    }
}
