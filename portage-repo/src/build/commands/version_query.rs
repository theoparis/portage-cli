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

fn vdb_root_for<SE: brush_core::ShellExtensions>(
    shell: &brush_core::Shell<SE>,
    broot: bool,
    sysroot: bool,
) -> std::path::PathBuf {
    let var = if broot {
        "BROOT"
    } else if sysroot {
        "ESYSROOT"
    } else {
        "ROOT"
    };
    let root = shell
        .env_str(var)
        .map(|s| s.into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/".to_string());
    std::path::Path::new(&root).join("var/db/pkg")
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
        let vdb = vdb_root_for(context.shell, self.broot, self.sysroot);
        let found = best_match(&vdb, &self.atom).is_some();
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
        let vdb = vdb_root_for(context.shell, self.broot, self.sysroot);
        match best_match(&vdb, &self.atom) {
            Some(cpv) => {
                let shell = context.shell;
                let _ = writeln!(context.params.stdout(shell), "{cpv}");
                Ok(brush_core::ExecutionResult::new(0))
            }
            None => Ok(brush_core::ExecutionResult::new(1)),
        }
    }
}
