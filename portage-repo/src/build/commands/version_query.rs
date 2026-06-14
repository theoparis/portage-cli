//! `has_version` / `best_version` builtins (PMS 12.3.13 / 12.3.4).
//!
//! Query the installed-package database for an atom. `-r` (default) queries
//! `ROOT`'s VDB, `-d` `ESYSROOT`'s, `-b` `BROOT`'s — under a `--prefix` run
//! these differ: build-time tools (`-b`, e.g. autotools.eclass probing the
//! installed autoconf) live on the host, while runtime deps live in the
//! prefix.

use std::io::Write;

use brush_core::builtins;
use clap::Parser;

fn vdb_roots_for<SE: brush_core::ShellExtensions>(
    shell: &brush_core::Shell<SE>,
    broot: bool,
    sysroot: bool,
) -> Vec<std::path::PathBuf> {
    let get = |var: &str| {
        shell
            .env_str(var)
            .map(|s| s.into_owned())
            .filter(|s| !s.is_empty())
    };
    let var = if broot {
        "BROOT"
    } else if sysroot {
        "ESYSROOT"
    } else {
        "ROOT"
    };
    let root = get(var).unwrap_or_else(|| "/".to_string());
    let mut roots = vec![root.clone()];
    // In-place `--local` (EPREFIX set): the run installs every package — build
    // tools and libraries alike — into the prefix EROOT, which is layered on
    // the host. So `-b`/`-d`/`-r` queries must also see the prefix, e.g.
    // python-any-r1's `has_version -b xcb-proto` where xcb-proto was just built
    // into the prefix, not the host. (Non-prefix builds have EROOT == ROOT, so
    // this adds nothing.)
    if get("EPREFIX").is_some()
        && let Some(eroot) = get("EROOT")
        && eroot != root
    {
        roots.push(eroot);
    }
    roots
        .into_iter()
        .map(|r| std::path::Path::new(&r).join("var/db/pkg"))
        .collect()
}

/// Best installed cpv matching `atom` across any of `vdb_paths`.
fn best_match_any(vdb_paths: &[std::path::PathBuf], atom: &str) -> Option<portage_atom::Cpv> {
    vdb_paths
        .iter()
        .filter_map(|p| best_match(p, atom))
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// Best installed cpv matching `atom` in the VDB at `vdb_path`, if any.
fn best_match(vdb_path: &std::path::Path, atom: &str) -> Option<portage_atom::Cpv> {
    let dep = portage_atom::Dep::parse(atom).ok()?;
    let vdb_path = camino::Utf8Path::from_path(vdb_path)?;
    let vdb = portage_vdb::Vdb::open(vdb_path).ok()?;
    let cat = vdb.category(dep.cpn.category.as_str())?;
    let mut best: Option<portage_atom::Cpv> = None;
    for pkg in cat.packages() {
        let cpv = pkg.cpv();
        if cpv.cpn != dep.cpn {
            continue;
        }
        let slot = pkg.slot_main().ok();
        if dep.matches_cpv(cpv, slot.as_deref())
            && best.as_ref().is_none_or(|b| cpv.version > b.version)
        {
            best = Some(cpv.clone());
        }
    }
    best
}

/// `has_version [-b|-d|-r] <atom>` — exit 0 when an installed package
/// matches.
#[derive(Parser)]
pub(crate) struct HasVersionCommand {
    /// Query BROOT (build tools).
    #[arg(short = 'b')]
    broot: bool,
    /// Query ESYSROOT.
    #[arg(short = 'd')]
    sysroot: bool,
    /// Query ROOT (the default).
    #[arg(short = 'r')]
    root: bool,
    atom: String,
}

impl builtins::Command for HasVersionCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let vdbs = vdb_roots_for(context.shell, self.broot, self.sysroot);
        let found = best_match_any(&vdbs, &self.atom).is_some();
        Ok(brush_core::ExecutionResult::new(u8::from(!found)))
    }
}

/// `best_version [-b|-d|-r] <atom>` — print the best matching installed cpv.
#[derive(Parser)]
pub(crate) struct BestVersionCommand {
    /// Query BROOT (build tools).
    #[arg(short = 'b')]
    broot: bool,
    /// Query ESYSROOT.
    #[arg(short = 'd')]
    sysroot: bool,
    /// Query ROOT (the default).
    #[arg(short = 'r')]
    root: bool,
    atom: String,
}

impl builtins::Command for BestVersionCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let vdbs = vdb_roots_for(context.shell, self.broot, self.sysroot);
        match best_match_any(&vdbs, &self.atom) {
            Some(cpv) => {
                let shell = context.shell;
                let _ = writeln!(context.params.stdout(shell), "{cpv}");
                Ok(brush_core::ExecutionResult::new(0))
            }
            None => Ok(brush_core::ExecutionResult::new(1)),
        }
    }
}
