use std::io::Write;

use brush_core::builtins;
use clap::Parser;

use super::die::DieFlag;

// ── P2 build helpers ──────────────────────────────────────────────────────────

/// `emake [args...]`  (PMS 12.3.2)
///
/// Runs `${MAKE:-make} ${MAKEOPTS} ${EXTRA_EMAKE} [args...]`.
#[derive(Parser)]
pub(crate) struct EmakeCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for EmakeCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let (stdout, stderr) = super::context_stdio(&context);
        // Forward the pipeline's stdin so `emake -f -` can read a makefile from
        // a pipe (toolchain.eclass's `get_make_var`); without it make sees no
        // makefile and stops with "No targets".
        let stdin = super::context_stdin(&context);
        // make (and any pkg-config/linker it spawns) must see the full build
        // environment, including bashrc-exported overlay search paths.
        let env_vars = super::context_env(&context);
        // Capture the shared die flag before the shell is moved out of context;
        // emake self-dies on a failed make (PMS 12.3.2) so eclasses that call
        // bare `emake` (no `|| die`) still abort — and the phase driver does not
        // have to treat a phase's trailing exit status as fatal (which would
        // mis-fire on benign tail commands like `find … -exec rmdir`).
        let die_flag = context.shared::<DieFlag>().ok().cloned();
        let shell = context.shell;
        // `nonfatal emake` (PORTAGE_NONFATAL) suppresses the self-die.
        let nonfatal = shell.env_str("PORTAGE_NONFATAL").as_deref() == Some("1");
        let make = shell
            .env_str("MAKE")
            .map(|s| s.into_owned())
            .unwrap_or_else(|| "make".to_string());
        let makeopts_str = shell
            .env_str("MAKEOPTS")
            .map(|s| s.into_owned())
            .unwrap_or_default();
        let extra_str = shell
            .env_str("EXTRA_EMAKE")
            .map(|s| s.into_owned())
            .unwrap_or_default();
        let makeopts: Vec<String> = makeopts_str
            .split_whitespace()
            .map(|s| s.to_owned())
            .collect();
        let extra: Vec<String> = extra_str.split_whitespace().map(|s| s.to_owned()).collect();
        let args = self.args.clone();
        let cwd = shell.working_dir().to_path_buf();

        let exit = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&make)
                .current_dir(&cwd)
                .args(&makeopts)
                .args(&extra)
                .args(&args)
                .envs(env_vars)
                .stdin(stdin)
                .stdout(stdout)
                .stderr(stderr)
                .status()
                .map(|s| s.code().unwrap_or(1) as u8)
                .unwrap_or(127)
        })
        .await
        .unwrap_or(127);

        // PMS: emake aborts the build when make fails. Raise the shared die flag
        // (checked by the phase driver after the phase returns) so a failed make
        // can't silently proceed.
        if exit != 0 && !nonfatal {
            let msg = format!("emake failed (make exited {exit})");
            if let Some(flag) = &die_flag {
                flag.raise(&msg);
            }
            let _ = writeln!(context.params.stderr(shell), "die: {msg}");
        }

        Ok(brush_core::ExecutionResult::new(exit))
    }
}
