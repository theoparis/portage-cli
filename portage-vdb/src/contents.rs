//! Parser for the VDB `CONTENTS` file.
//!
//! Format (one entry per line):
//! ```text
//! obj /path/to/file md5hash mtime
//! dir /path/to/dir
//! sym /path/to/link -> target mtime
//! fif /path/to/pipe
//! dev /path/to/device
//! ```

use camino::Utf8PathBuf;

/// Kind of filesystem entry in a CONTENTS file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentsKind {
    /// Regular file (`obj`). Carries an MD5 hash and mtime.
    Obj,
    /// Directory (`dir`).
    Dir,
    /// Symbolic link (`sym`). Carries a target path and mtime.
    Sym,
    /// FIFO/pipe (`fif`).
    Fifo,
    /// Device node (`dev`).
    Dev,
}

/// A single entry from a `CONTENTS` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentsEntry {
    /// Kind of entry (obj, dir, sym, fif, dev).
    pub kind: ContentsKind,
    /// Absolute path of the installed file/directory/symlink.
    pub path: Utf8PathBuf,
    /// MD5 digest (only for `Obj` entries).
    pub md5: Option<String>,
    /// File size or symlink target mtime as a Unix timestamp.
    pub mtime: Option<u64>,
    /// Symlink target (only for `Sym` entries).
    pub target: Option<Utf8PathBuf>,
}

/// Serialize a slice of entries back to a CONTENTS file string.
pub fn format_contents(entries: &[ContentsEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        out.push_str(&e.format_line());
        out.push('\n');
    }
    out
}

impl ContentsEntry {
    /// Serialize this entry to a single CONTENTS line (no trailing newline).
    pub fn format_line(&self) -> String {
        match self.kind {
            ContentsKind::Obj => format!(
                "obj {} {} {}",
                self.path,
                self.md5.as_deref().unwrap_or("0"),
                self.mtime.unwrap_or(0),
            ),
            ContentsKind::Dir => format!("dir {}", self.path),
            ContentsKind::Sym => format!(
                "sym {} -> {} {}",
                self.path,
                self.target.as_deref().map(|p| p.as_str()).unwrap_or(""),
                self.mtime.unwrap_or(0),
            ),
            ContentsKind::Fifo => format!("fif {}", self.path),
            ContentsKind::Dev => format!("dev {}", self.path),
        }
    }

    /// Parse a single line from a CONTENTS file.
    ///
    /// Returns `None` for blank lines.
    pub fn parse_line(line: &str) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }

        let mut parts = line.splitn(5, ' ');
        let kind_str = parts.next()?;

        let kind = match kind_str {
            "obj" => ContentsKind::Obj,
            "dir" => ContentsKind::Dir,
            "sym" => ContentsKind::Sym,
            "fif" => ContentsKind::Fifo,
            "dev" => ContentsKind::Dev,
            _ => return None,
        };

        let path_str = parts.next()?;
        let path = Utf8PathBuf::from(path_str);

        match kind {
            ContentsKind::Obj => {
                let md5 = parts.next().map(|s| s.to_string());
                let mtime = parts.next().and_then(|s| s.parse().ok());
                Some(ContentsEntry {
                    kind,
                    path,
                    md5,
                    mtime,
                    target: None,
                })
            }
            ContentsKind::Sym => {
                // sym /link/path -> /target mtime
                let rest = line.strip_prefix("sym ")?;
                let (path_and_target, mtime_str) = rest.rsplit_once(' ')?;
                let (path_str, target_str) = path_and_target.split_once(" -> ")?;
                Some(ContentsEntry {
                    kind,
                    path: Utf8PathBuf::from(path_str),
                    md5: None,
                    mtime: mtime_str.parse().ok(),
                    target: Some(Utf8PathBuf::from(target_str)),
                })
            }
            ContentsKind::Dir | ContentsKind::Fifo | ContentsKind::Dev => Some(ContentsEntry {
                kind,
                path,
                md5: None,
                mtime: None,
                target: None,
            }),
        }
    }

    /// Parse a full CONTENTS file into entries.
    pub fn parse(contents: &str) -> Vec<Self> {
        contents.lines().filter_map(Self::parse_line).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_obj() {
        let entry = ContentsEntry::parse_line(
            "obj /etc/skel/.bashrc d210b9cd7fc07420736480f2062d7d7f 1778566175",
        )
        .unwrap();
        assert_eq!(entry.kind, ContentsKind::Obj);
        assert_eq!(entry.path, Utf8PathBuf::from("/etc/skel/.bashrc"));
        assert_eq!(
            entry.md5.as_deref(),
            Some("d210b9cd7fc07420736480f2062d7d7f")
        );
        assert_eq!(entry.mtime, Some(1778566175));
        assert!(entry.target.is_none());
    }

    #[test]
    fn parse_dir() {
        let entry = ContentsEntry::parse_line("dir /etc").unwrap();
        assert_eq!(entry.kind, ContentsKind::Dir);
        assert_eq!(entry.path, Utf8PathBuf::from("/etc"));
    }

    #[test]
    fn parse_sym() {
        let entry = ContentsEntry::parse_line("sym /bin/rbash -> bash 1778566174").unwrap();
        assert_eq!(entry.kind, ContentsKind::Sym);
        assert_eq!(entry.path, Utf8PathBuf::from("/bin/rbash"));
        assert_eq!(entry.target.as_deref(), Some(camino::Utf8Path::new("bash")));
        assert_eq!(entry.mtime, Some(1778566174));
    }

    #[test]
    fn parse_sym_absolute_target() {
        let entry =
            ContentsEntry::parse_line("sym /usr/lib/libfoo.so -> libfoo.so.1 1234567890").unwrap();
        assert_eq!(entry.kind, ContentsKind::Sym);
        assert_eq!(
            entry.target.as_deref(),
            Some(camino::Utf8Path::new("libfoo.so.1"))
        );
    }

    #[test]
    fn format_obj() {
        let e = ContentsEntry {
            kind: ContentsKind::Obj,
            path: Utf8PathBuf::from("/etc/foo"),
            md5: Some("abc123".to_string()),
            mtime: Some(100),
            target: None,
        };
        assert_eq!(e.format_line(), "obj /etc/foo abc123 100");
    }

    #[test]
    fn format_dir() {
        let e = ContentsEntry {
            kind: ContentsKind::Dir,
            path: Utf8PathBuf::from("/etc"),
            md5: None,
            mtime: None,
            target: None,
        };
        assert_eq!(e.format_line(), "dir /etc");
    }

    #[test]
    fn format_sym() {
        let e = ContentsEntry {
            kind: ContentsKind::Sym,
            path: Utf8PathBuf::from("/bin/sh"),
            md5: None,
            mtime: Some(200),
            target: Some(Utf8PathBuf::from("bash")),
        };
        assert_eq!(e.format_line(), "sym /bin/sh -> bash 200");
    }

    #[test]
    fn format_roundtrip() {
        let lines = [
            "obj /etc/foo abc123 100",
            "dir /etc",
            "sym /bin/sh -> bash 200",
        ];
        for line in lines {
            let parsed = ContentsEntry::parse_line(line).unwrap();
            assert_eq!(parsed.format_line(), line);
        }
    }

    #[test]
    fn format_contents_fn() {
        let entries = ContentsEntry::parse("dir /etc\nobj /etc/foo abc123 100\n");
        let out = format_contents(&entries);
        assert_eq!(out, "dir /etc\nobj /etc/foo abc123 100\n");
    }

    #[test]
    fn parse_empty_line() {
        assert!(ContentsEntry::parse_line("").is_none());
        assert!(ContentsEntry::parse_line("  ").is_none());
    }

    #[test]
    fn parse_full_contents() {
        let raw = "dir /etc\nobj /etc/foo abc123 100\nsym /etc/bar -> baz 200\n";
        let entries = ContentsEntry::parse(raw);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].kind, ContentsKind::Dir);
        assert_eq!(entries[1].kind, ContentsKind::Obj);
        assert_eq!(entries[2].kind, ContentsKind::Sym);
    }
}
