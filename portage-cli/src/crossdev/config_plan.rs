//! Diff-and-apply plan for crossdev's generated config files.
//!
//! `--init-target` used to write every file unconditionally and immediately,
//! with no way to preview or confirm it — unlike every other mutating `em`
//! path, which honours the global `-p`/`--pretend` and `-a`/`--ask` flags.
//! This collects the desired state of each file/symlink/dir as a
//! [`ConfigEntry`] (no I/O beyond validation), diffs it against what's
//! actually on disk, and only then previews (`-p`), confirms (`-a`), or
//! applies — the config-regeneration equivalent of a merge plan.

use std::io::Write;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use crate::cli::Cli;

use super::symlink_force;
use crate::util::write_if_absent;

/// One file/dir/symlink `init_target` wants in a particular state.
pub(super) enum ConfigEntry {
    /// Regenerated every run: em owns the full content, so a rewrite always
    /// wins over whatever is currently on disk.
    File { path: Utf8PathBuf, desired: String },
    /// Written only if nothing is there yet; an existing file (any content)
    /// is left alone — either it never legitimately drifts (a bare location
    /// string), or it may belong to something else entirely em must not
    /// clobber.
    CreateOnly { path: Utf8PathBuf, desired: String },
    /// A `Location::Alias` `[crossdev]` repos.conf entry: refreshed when it's
    /// recognisably em's own (has an `alias-target =` key) but stale, left
    /// alone when it's foreign (e.g. a real crossdev/eselect-managed
    /// physical overlay with a `location =` key instead).
    Alias {
        path: Utf8PathBuf,
        category: String,
        packages_line: String,
    },
    /// A directory that just needs to exist (e.g. an empty target VDB).
    Dir { path: Utf8PathBuf },
    /// A symlink that should point at `target`.
    Symlink {
        link: Utf8PathBuf,
        target: Utf8PathBuf,
    },
}

enum Change {
    Create,
    Update,
    Unchanged,
}

impl ConfigEntry {
    fn path(&self) -> &Utf8Path {
        match self {
            ConfigEntry::File { path, .. }
            | ConfigEntry::CreateOnly { path, .. }
            | ConfigEntry::Alias { path, .. }
            | ConfigEntry::Dir { path } => path,
            ConfigEntry::Symlink { link, .. } => link,
        }
    }

    fn change(&self) -> Change {
        match self {
            ConfigEntry::File { path, desired } => match std::fs::read_to_string(path) {
                Ok(existing) if &existing == desired => Change::Unchanged,
                Ok(_) => Change::Update,
                Err(_) => Change::Create,
            },
            ConfigEntry::CreateOnly { path, .. } => {
                if path.exists() {
                    Change::Unchanged
                } else {
                    Change::Create
                }
            }
            ConfigEntry::Alias {
                path,
                category,
                packages_line,
            } => match std::fs::read_to_string(path) {
                Ok(existing)
                    if existing.contains(&format!("alias-target = {category}"))
                        && existing.contains(&format!("alias-packages = {packages_line}")) =>
                {
                    Change::Unchanged
                }
                // Foreign (no `alias-target =` key at all) — never touch.
                Ok(existing) if !existing.contains("alias-target =") => Change::Unchanged,
                Ok(_) => Change::Update,
                Err(_) => Change::Create,
            },
            ConfigEntry::Dir { path } => {
                if path.is_dir() {
                    Change::Unchanged
                } else {
                    Change::Create
                }
            }
            ConfigEntry::Symlink { link, target } => match std::fs::read_link(link) {
                Ok(dst) if dst == *target.as_std_path() => Change::Unchanged,
                Ok(_) => Change::Update,
                Err(_) => Change::Create,
            },
        }
    }

    fn apply(&self) -> Result<()> {
        if let Some(parent) = self.path().parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {parent}"))?;
        }
        match self {
            ConfigEntry::File { path, desired } => {
                std::fs::write(path, desired).with_context(|| format!("writing {path}"))
            }
            ConfigEntry::CreateOnly { path, desired } => write_if_absent(path, desired),
            ConfigEntry::Alias {
                path,
                category,
                packages_line,
            } => {
                // Re-check foreign-ness at apply time too (defence in depth;
                // `change()` already filtered foreign entries out of the plan).
                if let Ok(existing) = std::fs::read_to_string(path)
                    && !existing.contains("alias-target =")
                {
                    return Ok(());
                }
                let body = format!(
                    "[crossdev]\nalias-source = gentoo\nalias-target = {category}\n\
                     alias-packages = {packages_line}\n"
                );
                std::fs::write(path, body).with_context(|| format!("writing {path}"))
            }
            ConfigEntry::Dir { path } => {
                std::fs::create_dir_all(path).with_context(|| format!("creating {path}"))
            }
            ConfigEntry::Symlink { link, target } => symlink_force(target, link),
        }
    }
}

/// Apply every entry unconditionally, no diff/preview/confirm — for a caller
/// that is already externally gated by `!globals.pretend` (the native
/// toolchain `--setup` path, which has its own, separately-established
/// pretend handling and doesn't need its own preview here).
pub(super) fn apply_now(entries: &[ConfigEntry]) -> Result<()> {
    for e in entries {
        e.apply()?;
    }
    Ok(())
}

/// What happened to a collected [`ConfigEntry`] plan.
pub(super) enum Outcome {
    /// Nothing to do (or `-p`: shown but not written).
    NothingToApply,
    /// `-p`: previewed only.
    Previewed,
    /// `-a`: user declined.
    Declined,
    /// Written for real.
    Applied,
}

impl Outcome {
    /// Whether the caller should go on to print its own "ready" summary —
    /// true only when something was genuinely written (or there was nothing
    /// to do in the first place, i.e. already up to date).
    pub(super) fn applied(&self) -> bool {
        matches!(self, Outcome::Applied | Outcome::NothingToApply)
    }
}

/// Diff `entries` against disk and, per `globals.pretend`/`globals.ask`,
/// preview, confirm, or apply them.
pub(super) fn apply(entries: Vec<ConfigEntry>, globals: &Cli) -> Result<Outcome> {
    let mut changed: Vec<(Utf8PathBuf, &'static str)> = Vec::new();
    for e in &entries {
        let verb = match e.change() {
            Change::Create => "create",
            Change::Update => "update",
            Change::Unchanged => continue,
        };
        changed.push((e.path().to_owned(), verb));
    }

    if changed.is_empty() {
        return Ok(Outcome::NothingToApply);
    }

    if globals.pretend || globals.ask {
        println!(">>> config changes:");
        for (path, verb) in &changed {
            println!("  {verb} {path}");
        }
    }
    if globals.pretend {
        return Ok(Outcome::Previewed);
    }
    if globals.ask && !confirm_config_write(changed.len())? {
        println!(">>> Quitting.");
        return Ok(Outcome::Declined);
    }

    for e in &entries {
        e.apply()?;
    }
    Ok(Outcome::Applied)
}

fn confirm_config_write(count: usize) -> Result<bool> {
    print!("\n>>> Would you like to write these {count} config file(s)? [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Ok(false);
    }
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    // `Cli` has `arg_required_else_help = true`, so at least one arg must
    // always be present — `--root /` is a harmless no-op default here.
    fn cli(args: &[&str]) -> Cli {
        let mut full = vec!["em", "--root", "/"];
        full.extend_from_slice(args);
        Cli::parse_from(full)
    }

    /// `-p`: nothing is written, even though there's a real change to make.
    #[test]
    fn pretend_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("make.conf")).unwrap();
        let entries = vec![ConfigEntry::File {
            path: path.clone(),
            desired: "CHOST=riscv64-unknown-linux-gnu\n".to_owned(),
        }];
        let outcome = apply(entries, &cli(&["-p"])).unwrap();
        assert!(matches!(outcome, Outcome::Previewed));
        assert!(!path.exists(), "pretend must not write {path}");
    }

    /// Neither `-p` nor `-a`: applies directly, no prompt needed.
    #[test]
    fn plain_run_applies_directly() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("nested/make.conf")).unwrap();
        let desired = "CHOST=riscv64-unknown-linux-gnu\n".to_owned();
        let entries = vec![ConfigEntry::File {
            path: path.clone(),
            desired: desired.clone(),
        }];
        let outcome = apply(entries, &cli(&[])).unwrap();
        assert!(matches!(outcome, Outcome::Applied));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), desired);
    }

    /// Nothing changed (content already matches): reported as
    /// `NothingToApply`, and `Outcome::applied()` treats that as "the
    /// caller's own 'ready' summary may print" (it's already in the desired
    /// state either way).
    #[test]
    fn no_change_is_reported_as_nothing_to_apply() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("make.conf")).unwrap();
        let desired = "CHOST=riscv64-unknown-linux-gnu\n".to_owned();
        std::fs::write(&path, &desired).unwrap();
        let entries = vec![ConfigEntry::File {
            path: path.clone(),
            desired,
        }];
        let outcome = apply(entries, &cli(&[])).unwrap();
        assert!(matches!(outcome, Outcome::NothingToApply));
        assert!(outcome.applied());
    }

    /// `CreateOnly` never overwrites an existing file's content, no matter
    /// how it differs from `desired`.
    #[test]
    fn create_only_never_overwrites_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("gentoo.conf")).unwrap();
        std::fs::write(&path, "[gentoo]\nlocation = /somewhere/else\n").unwrap();
        let entries = vec![ConfigEntry::CreateOnly {
            path: path.clone(),
            desired: "[gentoo]\nlocation = /var/db/repos/gentoo\n".to_owned(),
        }];
        apply(entries, &cli(&[])).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[gentoo]\nlocation = /somewhere/else\n"
        );
    }

    /// `Dir` creates a missing directory and is a no-op once it exists.
    #[test]
    fn dir_entry_creates_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("var/db/pkg")).unwrap();
        assert!(!path.is_dir());
        apply(vec![ConfigEntry::Dir { path: path.clone() }], &cli(&[])).unwrap();
        assert!(path.is_dir());
    }

    /// A foreign `Alias` entry (no `alias-target =` key) is reported
    /// unchanged and never overwritten.
    #[test]
    fn alias_entry_never_touches_a_foreign_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("crossdev.conf")).unwrap();
        let foreign = "[crossdev]\nlocation = /var/db/repos/crossdev\n".to_owned();
        std::fs::write(&path, &foreign).unwrap();
        let entries = vec![ConfigEntry::Alias {
            path: path.clone(),
            category: "cross-riscv64-unknown-linux-gnu".to_owned(),
            packages_line: "sys-devel/binutils".to_owned(),
        }];
        let outcome = apply(entries, &cli(&[])).unwrap();
        assert!(matches!(outcome, Outcome::NothingToApply));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), foreign);
    }
}
