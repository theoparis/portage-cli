//! USE-flag lookup for [`crate::DepEntry::evaluate_use`].

use std::collections::HashSet;

use crate::interner::{DefaultInterner, Interned};

/// Answers whether a USE flag is active.
///
/// [`crate::DepEntry::evaluate_use`] takes `&impl UseFlagLookup` so callers pass
/// a flag set (or `portage-atom-pubgrub`'s `UseConfig`) directly — no
/// per-call-site closure. [`Interned`] is [`Copy`]; lookups take it by value.
pub trait UseFlagLookup {
    /// Returns `true` when `flag` is enabled for dependency evaluation.
    fn use_flag_active(&self, flag: Interned<DefaultInterner>) -> bool;
}

impl UseFlagLookup for HashSet<Interned<DefaultInterner>> {
    fn use_flag_active(&self, flag: Interned<DefaultInterner>) -> bool {
        self.contains(&flag)
    }
}

impl UseFlagLookup for HashSet<&str> {
    fn use_flag_active(&self, flag: Interned<DefaultInterner>) -> bool {
        self.contains(flag.as_str())
    }
}

impl UseFlagLookup for HashSet<String> {
    fn use_flag_active(&self, flag: Interned<DefaultInterner>) -> bool {
        self.contains(flag.as_str())
    }
}

impl UseFlagLookup for [&str] {
    fn use_flag_active(&self, flag: Interned<DefaultInterner>) -> bool {
        self.iter().any(|s| flag == *s)
    }
}

impl<const N: usize> UseFlagLookup for [&str; N] {
    fn use_flag_active(&self, flag: Interned<DefaultInterner>) -> bool {
        self.as_slice().use_flag_active(flag)
    }
}
