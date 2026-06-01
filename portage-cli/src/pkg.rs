use camino::{Utf8Path, Utf8PathBuf};
use portage_atom::Dep;
use portage_repo::PackageConf;

use crate::cli::PkgCommand;
use crate::error::{Error, Result};

pub fn run(command: &PkgCommand) -> Result<()> {
    match command {
        PkgCommand::Use { atom, add, subtract, drop, path } => {
            edit_valued(atom, add, subtract, drop, path.as_deref(), "package.use")
        }
        PkgCommand::Keyword { atom, add, subtract, drop, path } => {
            edit_valued(atom, add, subtract, drop, path.as_deref(), "package.accept_keywords")
        }
        PkgCommand::Mask { atom, add, drop, path } => {
            edit_mask(atom, *add, *drop, path.as_deref())
        }
        PkgCommand::Env { atom, add, drop, path } => {
            edit_valued(atom, add, &[], drop, path.as_deref(), "package.env")
        }
    }
}

// ---------------------------------------------------------------------------
// Valued entries (package.use / package.accept_keywords / package.env)
// ---------------------------------------------------------------------------

fn edit_valued(
    atom_str: &str,
    add: &[String],
    subtract: &[String],
    drop: &[String],
    path_override: Option<&Utf8Path>,
    conf_name: &str,
) -> Result<()> {
    let atom = Dep::parse(atom_str)
        .map_err(|e| Error::Other(format!("invalid atom {atom_str:?}: {e}")))?;

    let base = Utf8Path::new("/etc/portage").join(conf_name);
    let no_edit = add.is_empty() && subtract.is_empty() && drop.is_empty();

    if base.is_dir() {
        let mut all = PackageConf::load_dir(&base)
            .map_err(|e| Error::Other(format!("reading {base}: {e}")))?;

        // Find which files already contain this atom.
        let matches: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(_, (_, pc))| pc.find(&atom).is_some())
            .map(|(i, _)| i)
            .collect();

        if no_edit {
            show_valued_dir(&all, &matches, &atom, conf_name);
            return Ok(());
        }

        match matches.len() {
            0 => {
                // New entry — write to path_override or <base>/<cat>-<pkg>.
                let target = resolve_new_path(&base, &atom, path_override);
                let mut pc = if target.exists() {
                    PackageConf::load_file(&target)
                        .map_err(|e| Error::Other(format!("reading {target}: {e}")))?
                } else {
                    PackageConf::parse(String::new())
                        .map_err(|e| Error::Other(format!("{e}")))?
                };
                let current: Vec<String> = vec![];
                let new_values = apply_flags(current, add, subtract, drop);
                if new_values.is_empty() {
                    println!("{conf_name}: no entry for {atom}");
                } else {
                    let refs: Vec<&str> = new_values.iter().map(String::as_str).collect();
                    pc.set(&atom, &refs);
                    pc.save(&target)
                        .map_err(|e| Error::Other(format!("writing {target}: {e}")))?;
                    println!("{atom} {}", new_values.join(" "));
                }
            }
            1 => {
                let idx = matches[0];
                let (ref file, ref mut pc) = all[idx];
                if let Some(path_override) = path_override {
                    let target = base.join(path_override);
                    if &target != file {
                        eprintln!("warning: entry found in {}, ignoring --path", file.file_name().unwrap_or("?"));
                    }
                }
                update_valued_entry(pc, file, &atom, add, subtract, drop, conf_name)?;
            }
            _ => {
                eprintln!("error: atom found in multiple files:");
                for &i in &matches {
                    eprintln!("  {}", all[i].0);
                }
                eprintln!("Specify --path to edit one explicitly.");
                return Err(Error::Other(format!("ambiguous entries for {atom}")));
            }
        }
    } else {
        // Single-file mode.
        let mut pc = if base.exists() {
            PackageConf::load_file(&base)
                .map_err(|e| Error::Other(format!("reading {base}: {e}")))?
        } else {
            PackageConf::parse(String::new())
                .map_err(|e| Error::Other(format!("{e}")))?
        };

        if no_edit {
            show_valued_single(&pc, &atom, &base, conf_name);
            return Ok(());
        }

        update_valued_entry(&mut pc, &base, &atom, add, subtract, drop, conf_name)?;
    }

    Ok(())
}

fn update_valued_entry(
    pc: &mut PackageConf,
    file: &Utf8Path,
    atom: &Dep,
    add: &[String],
    subtract: &[String],
    drop: &[String],
    conf_name: &str,
) -> Result<()> {
    // If multiple versioned entries share this CPN and the user gave a bare
    // CPN atom, editing just the first would silently ignore the others.
    // Require a more specific atom in that case.
    let all_entries: Vec<_> = pc.find_all(atom).collect();
    if all_entries.len() > 1 && atom.version.is_none() {
        eprintln!("error: multiple entries for {atom} in {}:", file.file_name().unwrap_or("?"));
        for e in &all_entries {
            let values: Vec<&str> = e.values().collect();
            if values.is_empty() {
                eprintln!("  {}", e.atom_raw());
            } else {
                eprintln!("  {} {}", e.atom_raw(), values.join(" "));
            }
        }
        eprintln!("Use a versioned atom to edit a specific entry.");
        return Err(Error::Other(format!("ambiguous CPN for {atom}")));
    }

    let current: Vec<String> = all_entries
        .into_iter()
        .next()
        .map(|e| e.values().map(str::to_owned).collect())
        .unwrap_or_default();

    let new_values = apply_flags(current, add, subtract, drop);

    if new_values.is_empty() {
        pc.remove(atom);
        println!("{conf_name}: removed entry for {atom}");
    } else {
        let refs: Vec<&str> = new_values.iter().map(String::as_str).collect();
        pc.set(atom, &refs);
        println!("{atom} {}", new_values.join(" "));
    }

    pc.save(file)
        .map_err(|e| Error::Other(format!("writing {file}: {e}")))
}

fn show_valued_dir(
    all: &[(Utf8PathBuf, PackageConf)],
    matches: &[usize],
    atom: &Dep,
    conf_name: &str,
) {
    if matches.is_empty() {
        println!("{conf_name}: no entry for {atom}");
        return;
    }
    for &i in matches.iter() {
        let (ref file, ref pc) = all[i];
        let fname = file.file_name().unwrap_or("?");
        for entry in pc.find_all(atom) {
            let values: Vec<&str> = entry.values().collect();
            if values.is_empty() {
                println!("[{fname}] {}", entry.atom_raw());
            } else {
                println!("[{fname}] {} {}", entry.atom_raw(), values.join(" "));
            }
        }
    }
}

fn show_valued_single(pc: &PackageConf, atom: &Dep, file: &Utf8Path, conf_name: &str) {
    let fname = file.file_name().unwrap_or("?");
    let mut found = false;
    for entry in pc.find_all(atom) {
        found = true;
        let values: Vec<&str> = entry.values().collect();
        if values.is_empty() {
            println!("[{fname}] {}", entry.atom_raw());
        } else {
            println!("[{fname}] {} {}", entry.atom_raw(), values.join(" "));
        }
    }
    if !found {
        println!("{conf_name}: no entry for {atom}");
    }
}

// ---------------------------------------------------------------------------
// Mask (presence-only entries)
// ---------------------------------------------------------------------------

fn edit_mask(
    atom_str: &str,
    add: bool,
    drop: bool,
    path_override: Option<&Utf8Path>,
) -> Result<()> {
    let atom = Dep::parse(atom_str)
        .map_err(|e| Error::Other(format!("invalid atom {atom_str:?}: {e}")))?;

    let base = Utf8Path::new("/etc/portage/package.mask");

    if base.is_dir() {
        let mut all = PackageConf::load_dir(base)
            .map_err(|e| Error::Other(format!("reading {base}: {e}")))?;

        let matches: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(_, (_, pc))| pc.find(&atom).is_some())
            .map(|(i, _)| i)
            .collect();

        if !add && !drop {
            // Show mode.
            if matches.is_empty() {
                println!("package.mask: {atom} is not masked");
            } else {
                for &i in &matches {
                    let fname = all[i].0.file_name().unwrap_or("?");
                    println!("masked in [{fname}]: {atom}");
                }
            }
            return Ok(());
        }

        if drop {
            match matches.len() {
                0 => println!("package.mask: {atom} not found"),
                1 => {
                    let (ref file, ref mut pc) = all[matches[0]];
                    pc.remove(&atom);
                    pc.save(file)
                        .map_err(|e| Error::Other(format!("writing {file}: {e}")))?;
                    println!("removed {atom} from {}", file.file_name().unwrap_or("?"));
                }
                _ => {
                    eprintln!("error: atom found in multiple files:");
                    for &i in &matches {
                        eprintln!("  {}", all[i].0);
                    }
                    eprintln!("Specify --path to edit one explicitly.");
                    return Err(Error::Other(format!("ambiguous mask entries for {atom}")));
                }
            }
        } else {
            // add
            let target = resolve_new_path(base, &atom, path_override);
            let mut pc = if target.exists() {
                PackageConf::load_file(&target)
                    .map_err(|e| Error::Other(format!("reading {target}: {e}")))?
            } else {
                PackageConf::parse(String::new())
                    .map_err(|e| Error::Other(format!("{e}")))?
            };
            if pc.find(&atom).is_some() {
                println!("package.mask: {atom} already masked in {}", target.file_name().unwrap_or("?"));
            } else {
                pc.set(&atom, &[]);
                pc.save(&target)
                    .map_err(|e| Error::Other(format!("writing {target}: {e}")))?;
                println!("masked {atom} in {}", target.file_name().unwrap_or("?"));
            }
        }
    } else {
        let mut pc = if base.exists() {
            PackageConf::load_file(base)
                .map_err(|e| Error::Other(format!("reading {base}: {e}")))?
        } else {
            PackageConf::parse(String::new())
                .map_err(|e| Error::Other(format!("{e}")))?
        };

        if !add && !drop {
            if pc.find(&atom).is_some() {
                println!("package.mask: {atom} is masked");
            } else {
                println!("package.mask: {atom} is not masked");
            }
            return Ok(());
        }

        if drop {
            if pc.remove(&atom) {
                pc.save(base)
                    .map_err(|e| Error::Other(format!("writing {base}: {e}")))?;
                println!("removed {atom} from package.mask");
            } else {
                println!("package.mask: {atom} not found");
            }
        } else {
            pc.set(&atom, &[]);
            pc.save(base)
                .map_err(|e| Error::Other(format!("writing {base}: {e}")))?;
            println!("masked {atom}");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Merge add/subtract/drop into an existing flag list.
///
/// - add:      remove any existing `flag` and `-flag`, then push `flag`
/// - subtract: remove any existing `flag` and `-flag`, then push `-flag`
/// - drop:     remove both `flag` and `-flag` without adding anything
fn apply_flags(mut values: Vec<String>, add: &[String], subtract: &[String], drop: &[String]) -> Vec<String> {
    for op in add.iter().chain(subtract).chain(drop) {
        let base = op.trim_start_matches('-');
        values.retain(|v| {
            let vbase = v.trim_start_matches('-');
            vbase != base
        });
    }
    for flag in add {
        let base = flag.trim_start_matches('-');
        values.push(base.to_owned());
    }
    for flag in subtract {
        let base = flag.trim_start_matches('-');
        values.push(format!("-{base}"));
    }
    values
}

/// Resolve the target path for a new entry under a directory.
///
/// If `path_override` names a relative path it is joined under `base_dir`.
/// Otherwise the default `<cat>-<pkg>` filename is used.
fn resolve_new_path(base_dir: &Utf8Path, atom: &Dep, path_override: Option<&Utf8Path>) -> Utf8PathBuf {
    if let Some(p) = path_override {
        if p.is_absolute() {
            return p.to_owned();
        }
        return base_dir.join(p);
    }
    let stem = format!("{}-{}", atom.cpn.category, atom.cpn.package);
    base_dir.join(stem)
}
