//! `em select compiler` — `gcc-config`/`eselect gcc` workalike.
//!
//! Manages compiler profile selection for gcc. Reads/writes env.d files and
//! creates symlinks similar to gcc-config. Supports grouping profiles by target
//! architecture and showing which is active per architecture.

use anyhow::Result;

use super::{env_d, Cli};
use crate::cli::CompilerAction;

/// GCC-specific profile type.
pub struct GccProfileType;

impl env_d::EnvDProfile for GccProfileType {
    fn module_name() -> &'static str {
        "compiler"
    }

    fn env_d_subdir() -> &'static str {
        "gcc"
    }

    fn global_env_prefix() -> &'static str {
        "04gcc-"
    }

    fn target_var_name() -> &'static str {
        "CTARGET="
    }
}

pub fn run(action: &CompilerAction, globals: &Cli) -> Result<()> {
    let target = match action {
        CompilerAction::List { target, .. } | CompilerAction::Show { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
        CompilerAction::Set { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
    };

    let base_dir = env_d::env_d_dir::<GccProfileType>(globals);

    match action {
        CompilerAction::List { .. } => env_d::run_list::<GccProfileType>(globals),
        CompilerAction::Show { .. } => env_d::run_show::<GccProfileType>(globals, &target),
        CompilerAction::Set { profile, .. } => {
            env_d::run_set::<GccProfileType>(globals, &target, profile, &base_dir)
        }
    }
}
