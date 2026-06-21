pub use anyhow::Result;

/// A `--pretend` resolve that surfaced required USE/mask changes.
///
/// The detailed change block was already printed (by the depgraph), so this is
/// carried as an error purely to drive a non-zero exit through the normal
/// `Result` flow. [`main`](crate::main) recognises it and exits `1` *quietly* —
/// no `error:` prefix — matching `emerge -p`, where the printed block is the
/// message. When the staged-build driver adds step context, the context is
/// shown (the bootstrap genuinely cannot proceed past a step needing an unmask).
#[derive(Debug, thiserror::Error)]
#[error("USE/mask changes are required to proceed (see above)")]
pub struct ConfigChangesNeeded;
