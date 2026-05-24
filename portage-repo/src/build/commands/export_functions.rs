use std::io::Write;

use brush_core::builtins;
use clap::Parser;

// ── EXPORT_FUNCTIONS ──────────────────────────────────────────────────────────

/// `EXPORT_FUNCTIONS <phase> [phase...]`  (PMS 10.2)
///
/// For each named phase, defines a wrapper function in the shell:
///   `${phase}() { ${ECLASS}_${phase} "$@"; }`
///
/// Ported from bash `eval` to a batched `run_string` call, eliminating the
/// bash for-loop + per-phase eval overhead.
#[derive(Parser)]
pub(crate) struct ExportFunctionsCommand {
    #[arg(required = true)]
    phases: Vec<String>,
}

impl builtins::Command for ExportFunctionsCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;

        let eclass = match shell.env_str("ECLASS") {
            Some(e) if !e.is_empty() => e.into_owned(),
            _ => {
                let _ = writeln!(
                    context.params.stderr(shell),
                    "die: EXPORT_FUNCTIONS called outside eclass scope"
                );
                return Ok(brush_core::ExecutionResult::new(1));
            }
        };

        // Build all wrapper definitions as a single script and parse once.
        let script: String = self
            .phases
            .iter()
            .map(|phase| format!("{phase}() {{ {eclass}_{phase} \"$@\"; }}\n"))
            .collect();

        let source_info = brush_core::SourceInfo::from("EXPORT_FUNCTIONS");
        let params = shell.default_exec_params();
        shell.run_string(&script, &source_info, &params).await?;

        Ok(brush_core::ExecutionResult::success())
    }
}
