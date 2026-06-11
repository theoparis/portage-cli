use std::borrow::Cow;
use std::collections::HashMap;

use portage_atom::interner::{DefaultInterner, Interned};
use portage_atom::{Cpv, Dep};

use crate::repository::IUseDefault;

/// How a single USE flag should be evaluated during dependency conversion.
///
/// See [PMS 8.2](https://projects.gentoo.org/pms/9/pms.html#use-flag-dependent-dependencies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseFlagState {
    /// The flag is ON — `flag? ( deps )` includes deps, `!flag? ( deps )` skips.
    Enabled,
    /// The flag is OFF — `flag? ( deps )` skips deps, `!flag? ( deps )` includes.
    Disabled,
    /// The caller cedes this flag to the solver — a `UseDecision` virtual package
    /// (versions `0`/`1`) is created and the solver picks its value subject to
    /// constraints (Level-C `REQUIRED_USE`).  `prefer` is the value the caller's
    /// policy would have produced; `choose_version` biases toward it so a ceded
    /// flag only flips when a constraint forces it (greedy keep-configured).
    SolverDecided { prefer: bool },
}

/// Configuration for USE flag evaluation during dependency conversion.
///
/// See [PMS 8.2](https://projects.gentoo.org/pms/9/pms.html#use-flag-dependent-dependencies).
#[derive(Debug, Clone, Default)]
pub struct UseConfig {
    flags: HashMap<Interned<DefaultInterner>, UseFlagState>,
}

impl UseConfig {
    /// Create an empty config (all flags default to `Disabled`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a flag's state.
    pub fn set(&mut self, flag: Interned<DefaultInterner>, state: UseFlagState) {
        self.flags.insert(flag, state);
    }

    /// Enable a flag.
    pub fn enable(&mut self, flag: Interned<DefaultInterner>) {
        self.flags.insert(flag, UseFlagState::Enabled);
    }

    /// Disable a flag.
    pub fn disable(&mut self, flag: Interned<DefaultInterner>) {
        self.flags.insert(flag, UseFlagState::Disabled);
    }

    /// Mark a flag as solver-decided, with the caller's preferred value.
    pub fn solver_decide(&mut self, flag: Interned<DefaultInterner>, prefer: bool) {
        self.flags
            .insert(flag, UseFlagState::SolverDecided { prefer });
    }

    /// Get the state of a flag. Unset flags default to `Disabled`.
    pub fn get(&self, flag: &Interned<DefaultInterner>) -> UseFlagState {
        self.flags
            .get(flag)
            .copied()
            .unwrap_or(UseFlagState::Disabled)
    }

    /// Return `Some(state)` if the flag is explicitly set, `None` if absent.
    pub fn get_opt(&self, flag: &Interned<DefaultInterner>) -> Option<UseFlagState> {
        self.flags.get(flag).copied()
    }

    /// Get the state of a flag, falling back to an IUSE default if the flag
    /// is not explicitly configured.
    ///
    /// If the flag is set in the config, returns its state. Otherwise, if the
    /// IUSE default is `Enabled` (the `+` prefix), returns `Enabled`.
    /// Otherwise returns `Disabled`.
    pub fn get_with_iuse_default(
        &self,
        flag: &Interned<DefaultInterner>,
        iuse_default: Option<IUseDefault>,
    ) -> UseFlagState {
        match self.flags.get(flag) {
            Some(&state) => state,
            None => match iuse_default {
                Some(IUseDefault::Enabled) => UseFlagState::Enabled,
                _ => UseFlagState::Disabled,
            },
        }
    }

    /// Fold a version's IUSE defaults into this config: for every flag not
    /// already set explicitly, apply its `+`/`-` default.  After this, the
    /// config is an authoritative "desired" set — a plain `get()` gives the
    /// flag's effective state with no separate default lookup needed.
    pub fn fold_iuse_defaults(
        &mut self,
        defaults: &HashMap<Interned<DefaultInterner>, IUseDefault>,
    ) {
        for (flag, def) in defaults {
            if !self.flags.contains_key(flag) {
                self.flags.insert(
                    *flag,
                    match def {
                        IUseDefault::Enabled => UseFlagState::Enabled,
                        IUseDefault::Disabled => UseFlagState::Disabled,
                    },
                );
            }
        }
    }

    /// Returns all solver-decided flags.
    /// Every flag this config explicitly enables (sorted, for stable output).
    pub fn enabled_flags(&self) -> Vec<Interned<DefaultInterner>> {
        let mut v: Vec<Interned<DefaultInterner>> = self
            .flags
            .iter()
            .filter(|(_, s)| matches!(s, UseFlagState::Enabled))
            .map(|(f, _)| *f)
            .collect();
        v.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        v
    }

    pub fn solver_decided_flags(&self) -> Vec<Interned<DefaultInterner>> {
        self.flags
            .iter()
            .filter(|(_, s)| matches!(s, UseFlagState::SolverDecided { .. }))
            .map(|(f, _)| *f)
            .collect()
    }
}

/// Apply per-package USE flag overrides on top of a base [`UseConfig`].
///
/// Scans `package_use` in order and applies any entries whose atom matches
/// `cpv`.  Returns `Borrowed(base)` when no entries match to avoid a clone.
/// This is policy resolution the *caller* performs to build the desired set;
/// the solver itself never calls it.
pub fn apply_package_use<'a>(
    base: &'a UseConfig,
    cpv: &Cpv,
    slot: Option<Interned<DefaultInterner>>,
    package_use: &[(Dep, Vec<String>)],
) -> Cow<'a, UseConfig> {
    if package_use.is_empty() {
        return Cow::Borrowed(base);
    }
    let mut cfg = base.clone();
    for (dep, flags) in package_use {
        if crate::validate::dep_matches_cpv(dep, cpv, slot) {
            for flag in flags {
                let name = flag.strip_prefix('+').unwrap_or(flag);
                if let Some(stripped) = name.strip_prefix('-') {
                    cfg.disable(Interned::intern(stripped));
                } else {
                    cfg.enable(Interned::intern(name));
                }
            }
        }
    }
    Cow::Owned(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_defaults_to_disabled() {
        let config = UseConfig::new();
        let flag = Interned::intern("ssl");
        assert_eq!(config.get(&flag), UseFlagState::Disabled);
    }

    #[test]
    fn enable_disable() {
        let mut config = UseConfig::new();
        let flag = Interned::intern("ssl");
        config.enable(flag);
        assert_eq!(config.get(&flag), UseFlagState::Enabled);
        config.disable(flag);
        assert_eq!(config.get(&flag), UseFlagState::Disabled);
    }

    #[test]
    fn solver_decided_flags() {
        let mut config = UseConfig::new();
        let ssl = Interned::intern("ssl");
        let debug = Interned::intern("debug");
        let test = Interned::intern("test");
        config.enable(ssl);
        config.solver_decide(debug, false);
        config.solver_decide(test, true);
        let decided = config.solver_decided_flags();
        assert_eq!(decided.len(), 2);
    }

    #[test]
    fn set_method() {
        let mut config = UseConfig::new();
        let flag = Interned::intern("ssl");
        config.set(flag, UseFlagState::Enabled);
        assert_eq!(config.get(&flag), UseFlagState::Enabled);
        config.set(flag, UseFlagState::SolverDecided { prefer: false });
        assert_eq!(
            config.get(&flag),
            UseFlagState::SolverDecided { prefer: false }
        );
    }

    #[test]
    fn get_with_iuse_default_none_returns_disabled() {
        let config = UseConfig::new();
        let flag = Interned::intern("ssl");
        assert_eq!(
            config.get_with_iuse_default(&flag, None),
            UseFlagState::Disabled
        );
    }

    #[test]
    fn get_with_iuse_default_disabled() {
        let config = UseConfig::new();
        let flag = Interned::intern("ssl");
        assert_eq!(
            config.get_with_iuse_default(&flag, Some(IUseDefault::Disabled)),
            UseFlagState::Disabled
        );
    }

    #[test]
    fn get_with_iuse_default_overridden_by_config() {
        let mut config = UseConfig::new();
        let flag = Interned::intern("ssl");
        config.disable(flag);
        assert_eq!(
            config.get_with_iuse_default(&flag, Some(IUseDefault::Enabled)),
            UseFlagState::Disabled
        );
    }

    #[test]
    fn solver_decided_flags_empty() {
        let config = UseConfig::new();
        assert!(config.solver_decided_flags().is_empty());
    }
}
