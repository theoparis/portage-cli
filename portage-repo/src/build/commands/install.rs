//! `do*` / `new*` install helpers as Rust builtins (PMS 12.3.x).
//!
//! These replace the hand-parsed bash versions in `INSTALL_HELPERS`: real clap
//! arg parsing (`-r`, the `-` = stdin convention), `${ED}`/dest-tree awareness,
//! and the shared [`DieFlag`](super::die::DieFlag) on failure. They shell out to
//! coreutils `install`/`cp` for byte-identical mode/ownership semantics — the
//! win is removing fragile bash, not reimplementing install(1).
//!
//! Conversion is incremental; helpers still in `INSTALL_HELPERS` keep working
//! alongside the ones registered here (a registered builtin shadows the bash
//! function of the same name).

use std::io::Write;
use std::path::PathBuf;

use brush_core::builtins;
use clap::Parser;

use super::die::DieFlag;

/// The image staging root files install into: `${ED}` (= `${D}${EPREFIX}`),
/// trailing slash trimmed. Falls back to `${D}` then `/` (should not happen
/// once `init_build_env` has run).
fn ed<SE: brush_core::ShellExtensions>(shell: &brush_core::Shell<SE>) -> PathBuf {
    for var in ["ED", "D"] {
        if let Some(v) = shell.env_str(var).filter(|s| !s.is_empty()) {
            return PathBuf::from(v.trim_end_matches('/'));
        }
    }
    PathBuf::from("")
}

fn var<SE: brush_core::ShellExtensions>(
    shell: &brush_core::Shell<SE>,
    name: &str,
    default: &str,
) -> String {
    shell
        .env_str(name)
        .map(|s| s.into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Raise the shared die flag with `msg` and return exit status 1, matching the
/// bash helpers' `|| die "..."`.
fn raise_die<SE: brush_core::ShellExtensions>(
    context: &brush_core::ExecutionContext<'_, SE>,
    msg: &str,
) -> brush_core::ExecutionResult {
    if let Ok(flag) = context.shared::<DieFlag>() {
        flag.raise(msg);
    }
    let _ = writeln!(context.params.stderr(context.shell), "die: {msg}");
    brush_core::ExecutionResult::new(1)
}

/// `doins [-r] <file>...` (PMS 12.3.4): install files into `${ED}${INSDESTTREE}`
/// with `INSOPTIONS` (default `-m0644`); `-r` recurses into directories.
#[derive(Parser)]
pub(crate) struct DoinsCommand {
    #[arg(short = 'r')]
    recursive: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    files: Vec<String>,
}

impl builtins::Command for DoinsCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        if self.files.is_empty() {
            return Ok(raise_die(&context, "doins: at least one argument required"));
        }
        let env = super::context_env(&context);

        let insdest = var(context.shell, "INSDESTTREE", "");
        let insopts = var(context.shell, "_insopts", "-m0644");
        let dest = ed(context.shell).join(insdest.trim_start_matches('/'));
        // Relative source args resolve against the shell's CWD (${S}), not em's
        // process CWD — so spawned install/cp must run there.
        let cwd = context.shell.working_dir().to_path_buf();
        let recursive = self.recursive;
        let files = self.files.clone();

        let result = tokio::task::spawn_blocking(move || {
            use std::process::Command;
            if let Err(e) = std::fs::create_dir_all(&dest) {
                return Err(format!("doins: creating {}: {e}", dest.display()));
            }
            for f in &files {
                let src = cwd.join(f);
                // install/cp inherit em's stdio (they're silent on success; the
                // failure path reports via raise_die on the shell's stderr fd).
                let status = if recursive && src.is_dir() {
                    Command::new("cp")
                        .arg("-pPR")
                        .arg(&src)
                        .arg(format!("{}/", dest.display()))
                        .envs(env.iter().cloned())
                        .status()
                } else {
                    let name = src.file_name().map(|n| n.to_owned()).unwrap_or_default();
                    let target = dest.join(name);
                    let mut cmd = Command::new("install");
                    for opt in insopts.split_whitespace() {
                        cmd.arg(opt);
                    }
                    cmd.arg(&src)
                        .arg(&target)
                        .envs(env.iter().cloned())
                        .status()
                };
                match status {
                    Ok(s) if s.success() => {}
                    _ => return Err(format!("doins: failed to install {f}")),
                }
            }
            Ok(())
        })
        .await
        .unwrap_or_else(|e| Err(format!("doins: task failed: {e}")));

        match result {
            Ok(()) => Ok(brush_core::ExecutionResult::success()),
            Err(msg) => Ok(raise_die(&context, &msg)),
        }
    }
}
