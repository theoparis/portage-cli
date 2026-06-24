//! `em select binutils` — `binutils-config`/`eselect binutils` workalike.
//!
//! Manages binutils profile selection. Supports grouping profiles by target
//! architecture and showing which is active per architecture.

use anyhow::Result;

use super::{env_d, Cli};
use crate::cli::BinutilsAction;

/// Binutils-specific profile type.
pub struct BinutilsProfileType;

impl env_d::EnvDProfile for BinutilsProfileType {
    fn module_name() -> &'static str {
        "binutils"
    }

    fn env_d_subdir() -> &'static str {
        "binutils"
    }

    fn global_env_prefix() -> &'static str {
        "05binutils"
    }

    fn target_var_name() -> &'static str {
        "TARGET="
    }
}

pub fn run(action: &BinutilsAction, globals: &Cli) -> Result<()> {
    let target = match action {
        BinutilsAction::List { target, .. } | BinutilsAction::Show { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
        BinutilsAction::Set { target, .. } => target
            .clone()
            .unwrap_or_else(|| env_d::get_default_target(globals)),
    };

    let base_dir = env_d::env_d_dir::<BinutilsProfileType>(globals);

    match action {
        BinutilsAction::List { .. } => env_d::run_list::<BinutilsProfileType>(globals),
        BinutilsAction::Show { .. } => env_d::run_show::<BinutilsProfileType>(globals, &target),
        BinutilsAction::Set { profile, .. } => {
            env_d::run_set::<BinutilsProfileType>(globals, &target, profile, &base_dir)
        }
    }
}
