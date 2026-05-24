use std::path::Path;

use crate::error::{Error, Result};

/// Create an `Error::Io` from a path and an `io::Error`.
pub(crate) fn io_err(path: impl AsRef<Path>, source: std::io::Error) -> Error {
    Error::Io {
        path: path.as_ref().to_path_buf(),
        source,
    }
}

/// Read a file to a string, mapping I/O errors to `Error::Io`.
pub(crate) fn read_to_string(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    std::fs::read_to_string(path).map_err(|e| io_err(path, e))
}

/// Read non-blank, non-comment lines from a file.
///
/// Lines starting with `#` (after trimming) are treated as comments.
/// Returns an empty `Vec` if the file does not exist.
pub(crate) fn read_lines(path: impl AsRef<Path>) -> Result<Vec<String>> {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(io_err(path, e)),
    }
}

/// Read the first non-blank, non-comment line from a file.
///
/// Returns `None` if the file does not exist.
pub(crate) fn read_single_line(path: impl AsRef<Path>) -> Result<Option<String>> {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_lines_skips_blanks_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "# comment").unwrap();
        writeln!(f, "").unwrap();
        writeln!(f, "  alpha  ").unwrap();
        writeln!(f, "# another comment").unwrap();
        writeln!(f, "beta").unwrap();

        let lines = read_lines(&path).unwrap();
        assert_eq!(lines, vec!["alpha", "beta"]);
    }

    #[test]
    fn read_lines_missing_file_returns_empty() {
        let lines = read_lines(Path::new("/nonexistent/path/file.txt")).unwrap();
        assert!(lines.is_empty());
    }

    #[test]
    fn read_single_line_returns_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "# comment\n\nfirst\nsecond\n").unwrap();

        let line = read_single_line(&path).unwrap();
        assert_eq!(line.as_deref(), Some("first"));
    }

    #[test]
    fn read_single_line_missing_returns_none() {
        let line = read_single_line(Path::new("/nonexistent/path/file.txt")).unwrap();
        assert!(line.is_none());
    }
}
