//! Profile USE *force* and *mask*, resolved per package.
//!
//! Global `use.force`/`use.mask` are already folded into the base `UseConfig`
//! by `portage-repo`'s `resolve_use_flags`, so they are kept here only for the
//! Level-C cede gate (a globally pinned flag must never be ceded). The
//! **package-level** sets (`package.use.force`/`package.use.mask`) and all the
//! **`*.stable.*`** variants are otherwise applied *nowhere*; this module layers
//! them onto a package's effective USE.
//!
//! The `.stable.*` files only influence a package "merged due to a stable
//! keyword" (portage(5)); [`is_stable`] reproduces Portage's
//! `KeywordsManager.isStable`. On a `~arch` `ACCEPT_KEYWORDS` every package is
//! merged via an unstable keyword, so the stable sets never apply there — they
//! matter on pure-stable systems.
//!
//! This is what makes cross-compilation targets resolve correctly: crossdev
//! ships `/etc/portage/profile/package.use.{force,mask}/cross-*` pinning
//! `multilib`/`cet`/`nopie`, which only take effect once package-level force/mask
//! are applied to effective USE.

use std::collections::BTreeSet;

use portage_atom::Cpv;
use portage_atom::interner::Interned;
use portage_atom::Dep;
use portage_atom_pubgrub::UseConfig;
use portage_metadata::Keyword;

use super::repo::{keyword_accepts, mask_matches};

/// Resolved profile force/mask policy. The `Vec<String>` globals are already
/// `-`-resolved (`merge_use_flags`); the per-atom sets keep raw tokens so a
/// `-flag` (unforce/unmask) is resolved against the accumulated set per package.
#[derive(Default)]
pub(super) struct ForceMask {
    pub use_force: Vec<String>,
    pub use_mask: Vec<String>,
    pub use_stable_force: Vec<String>,
    pub use_stable_mask: Vec<String>,
    pub pkg_force: Vec<(Dep, Vec<String>)>,
    pub pkg_mask: Vec<(Dep, Vec<String>)>,
    pub pkg_stable_force: Vec<(Dep, Vec<String>)>,
    pub pkg_stable_mask: Vec<(Dep, Vec<String>)>,
}

/// Accumulate per-atom flag entries matching `cpv` into `set`, honouring `-flag`
/// removal (incremental, in list order).
fn accumulate(entries: &[(Dep, Vec<String>)], cpv: &Cpv, set: &mut BTreeSet<String>) {
    for (dep, flags) in entries {
        if !mask_matches(dep, cpv) {
            continue;
        }
        for f in flags {
            if let Some(name) = f.strip_prefix('-') {
                set.remove(name);
            } else {
                set.insert(f.clone());
            }
        }
    }
}

impl ForceMask {
    /// The net forced and masked flag names for `cpv` that are **not** already in
    /// the base config — i.e. package-level force/mask always, plus the
    /// `*.stable.*` sets when `stable`. Global non-stable `use.force`/`use.mask`
    /// are excluded (they live in the base config). Mask wins over force.
    pub(super) fn effective(&self, cpv: &Cpv, stable: bool) -> (BTreeSet<String>, BTreeSet<String>) {
        let mut forced = BTreeSet::new();
        let mut masked = BTreeSet::new();
        accumulate(&self.pkg_force, cpv, &mut forced);
        accumulate(&self.pkg_mask, cpv, &mut masked);
        if stable {
            forced.extend(self.use_stable_force.iter().cloned());
            accumulate(&self.pkg_stable_force, cpv, &mut forced);
            masked.extend(self.use_stable_mask.iter().cloned());
            accumulate(&self.pkg_stable_mask, cpv, &mut masked);
        }
        forced.retain(|f| !masked.contains(f));
        (forced, masked)
    }

    /// Apply force/mask to a package's effective USE: enable forced flags, then
    /// disable masked ones (mask wins). Overrides `package.use` and the
    /// configured value, matching Portage.
    pub(super) fn apply(&self, cfg: &mut UseConfig, cpv: &Cpv, stable: bool) {
        let (forced, masked) = self.effective(cpv, stable);
        for f in &forced {
            cfg.enable(Interned::intern(f));
        }
        for f in &masked {
            cfg.disable(Interned::intern(f));
        }
    }

    /// Every flag name pinned for `cpv` (global force/mask + the package-level and
    /// stable sets) — the Level-C cede gate must never cede any of these.
    pub(super) fn pins(&self, cpv: &Cpv, stable: bool) -> BTreeSet<String> {
        let (mut pins, masked) = self.effective(cpv, stable);
        pins.extend(masked);
        pins.extend(self.use_force.iter().cloned());
        pins.extend(self.use_mask.iter().cloned());
        pins
    }

    /// Whether `cpv` carries any force/mask entries at all (cheap skip for the
    /// common no-policy case).
    pub(super) fn is_empty(&self) -> bool {
        self.use_force.is_empty()
            && self.use_mask.is_empty()
            && self.use_stable_force.is_empty()
            && self.use_stable_mask.is_empty()
            && self.pkg_force.is_empty()
            && self.pkg_mask.is_empty()
            && self.pkg_stable_force.is_empty()
            && self.pkg_stable_mask.is_empty()
    }
}

/// Whether `cpv` is "stable" for the purpose of `*.stable.*` force/mask, i.e.
/// merged due to a stable keyword.
///
/// Mirrors Portage's `KeywordsManager.isStable`: the package must be accepted by
/// `accept_keywords`, **and** would become unacceptable if every keyword were
/// downgraded to its `~` form. For a single arch that reduces to: accepted *and*
/// `accept_keywords` does not accept testing (`~arch`, `~*`, or `**`). So on a
/// `~arch` configuration this is always `false`.
pub(super) fn is_stable(keywords: &[Keyword], arch: &str, accept_keywords: &[String]) -> bool {
    if !keyword_accepts(keywords, arch, accept_keywords) {
        return false;
    }
    let testing = format!("~{arch}");
    let accepts_testing = accept_keywords
        .iter()
        .any(|k| *k == testing || k == "~*" || k == "**");
    !accepts_testing
}

#[cfg(test)]
mod tests {
    use super::*;
    use portage_atom::Version;
    use portage_atom_pubgrub::UseFlagState;
    use portage_metadata::Stability;

    fn cpv(s: &str) -> Cpv {
        let (cpn, ver) = s.rsplit_once('-').unwrap();
        Cpv::new(
            portage_atom::Cpn::parse(cpn).unwrap(),
            Version::parse(ver).unwrap(),
        )
    }

    fn dep(s: &str) -> Dep {
        Dep::parse(s).unwrap()
    }

    fn kw(arch: &str, stability: Stability) -> Keyword {
        Keyword {
            arch: Interned::intern(arch),
            stability,
        }
    }

    #[test]
    fn package_force_and_mask_apply_with_mask_winning() {
        let fm = ForceMask {
            pkg_force: vec![(dep("cross-foo/gcc"), vec!["multilib".into(), "shared".into()])],
            pkg_mask: vec![(dep("cross-foo/gcc"), vec!["cet".into(), "shared".into()])],
            ..Default::default()
        };
        let c = cpv("cross-foo/gcc-13.2");
        let (forced, masked) = fm.effective(&c, false);
        assert!(forced.contains("multilib"));
        assert!(!forced.contains("shared"), "shared is masked → dropped from force");
        assert!(masked.contains("cet"));
        assert!(masked.contains("shared"));

        let mut cfg = UseConfig::new();
        cfg.enable(Interned::intern("cet")); // user tried to enable a masked flag
        fm.apply(&mut cfg, &c, false);
        assert_eq!(cfg.get(&Interned::intern("multilib")), UseFlagState::Enabled);
        assert_eq!(cfg.get(&Interned::intern("cet")), UseFlagState::Disabled);
        assert_eq!(cfg.get(&Interned::intern("shared")), UseFlagState::Disabled);
    }

    #[test]
    fn unforce_token_removes_from_set() {
        let fm = ForceMask {
            // parent forces multilib, leaf unforces it for this atom
            pkg_force: vec![
                (dep("cross-foo/gcc"), vec!["multilib".into()]),
                (dep("cross-foo/gcc"), vec!["-multilib".into()]),
            ],
            ..Default::default()
        };
        let (forced, _) = fm.effective(&cpv("cross-foo/gcc-13.2"), false);
        assert!(!forced.contains("multilib"), "-multilib unforced it");
    }

    #[test]
    fn stable_sets_only_apply_when_stable() {
        let fm = ForceMask {
            use_stable_mask: vec!["risky".into()],
            ..Default::default()
        };
        let c = cpv("dev-libs/foo-1");
        assert!(!fm.effective(&c, false).1.contains("risky"), "ignored when unstable");
        assert!(fm.effective(&c, true).1.contains("risky"), "applied when stable");
    }

    #[test]
    fn is_stable_follows_accept_keywords() {
        let keywords = [kw("arm64", Stability::Stable)];
        // pure-stable config → stable
        assert!(is_stable(&keywords, "arm64", &["arm64".into()]));
        // testing accepted → never stable
        assert!(!is_stable(&keywords, "arm64", &["arm64".into(), "~arm64".into()]));
        assert!(!is_stable(&keywords, "arm64", &["~arm64".into()]));
        // not accepted at all → not stable
        assert!(!is_stable(&keywords, "arm64", &["amd64".into()]));
    }
}
