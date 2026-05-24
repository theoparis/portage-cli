use std::io::Write;

use brush_core::builtins;
use clap::Parser;

// ── die ───────────────────────────────────────────────────────────────────────

/// `die [message]`  (PMS 12.2.1)
///
/// Prints `die: <message>` to stderr and returns 1.
/// The "die: " prefix is load-bearing — it is matched by tests and by
/// `inherit` error paths to distinguish portage die output from other stderr.
#[derive(Parser)]
pub(crate) struct DieCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    message: Vec<String>,
}

impl builtins::Command for DieCommand {
    type State = ();
    type SharedState = ();
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let shell = context.shell;
        let msg = self.message.join(" ");
        let _ = writeln!(context.params.stderr(shell), "die: {msg}");
        Ok(brush_core::ExecutionResult::new(1))
    }
}
