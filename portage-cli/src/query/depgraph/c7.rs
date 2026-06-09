//! Corner-case spec for **C7 — cross-package `[flag]` USE-dep co-solve**.
//!
//! These are minimal package sets that exercise the scenarios C7 must handle.
//! Today a cross-package `[flag]` dep is **Tier 2**: the solver *detects* the
//! requirement (`PortageDependencyProvider::use_flag_requirements`) and the cli
//! turns it into an autounmask `package.use` suggestion, but the flag is **not**
//! applied — the plan is emitted with the flag unchanged.
//!
//! The goal of C7 (under `--autosolve-use`, mirroring Level-C `REQUIRED_USE`) is
//! to **cede** the target flag and re-solve to a fixpoint, so the requirement is
//! satisfied in the plan rather than only suggested. Each test below asserts the
//! *current* behaviour and documents the C7 target in a `C7:` comment, so the
//! switch to co-solve is a localized assertion change plus the implementation.
//!
//! The default (no `--autosolve-use`) must keep matching `emerge -p`, which also
//! only advises USE changes — so these cases stay Tier 2 unless autosolve is on.

#![cfg(test)]

use gentoo_core::Arch;
use portage_atom::{Cpv, Dep};
use portage_atom_pubgrub::{
    PortageDependencyProvider, PortageVersionSet, UseConfig, UseFlagRequirement,
};
use portage_metadata::CacheEntry;
use std::collections::HashMap;

use super::force_mask::ForceMask;
use super::repo::{Adapter, RepoData, target_package};

/// Build a `RepoData` from `(cpv, md5-cache-text)` pairs.
fn repo_from(entries: &[(&str, &str)]) -> RepoData {
    let mut versions: HashMap<portage_atom::Cpn, Vec<(Cpv, CacheEntry)>> = HashMap::new();
    let mut cpns = Vec::new();
    for (cpv_str, text) in entries {
        let cpv = Cpv::parse(cpv_str).unwrap();
        let entry = CacheEntry::parse(text).unwrap();
        if !cpns.contains(&cpv.cpn) {
            cpns.push(cpv.cpn);
        }
        versions.entry(cpv.cpn).or_default().push((cpv, entry));
    }
    RepoData { cpns, versions, repo_name: "test".into() }
}

/// The outcome of a solve: the cross-package USE-flag requirements the solver
/// detected, and the real (non-virtual) packages in the plan as `cat/pkg-ver`.
struct Outcome {
    reqs: Vec<UseFlagRequirement>,
    plan: Vec<String>,
}

impl Outcome {
    fn req_for(&self, cpn: &str) -> Option<&UseFlagRequirement> {
        self.reqs.iter().find(|r| r.package.cpn().to_string() == cpn)
    }
    fn needs_enabled(&self, cpn: &str, flag: &str) -> bool {
        self.req_for(cpn)
            .is_some_and(|r| r.required_enabled.iter().any(|f| f.as_str() == flag))
    }
    fn needs_disabled(&self, cpn: &str, flag: &str) -> bool {
        self.req_for(cpn)
            .is_some_and(|r| r.required_disabled.iter().any(|f| f.as_str() == flag))
    }
    fn has(&self, cpv_prefix: &str) -> bool {
        self.plan.iter().any(|p| p.starts_with(cpv_prefix))
    }
}

/// Solve `targets` against `data`. Returns `Err` (as a string) for an
/// unsatisfiable problem.
fn solve(data: &RepoData, targets: &[&str], autosolve: bool) -> Result<Outcome, String> {
    let arch = Arch::intern("amd64");
    let accept = ["amd64".to_string()];
    let lic = ["*".to_string()];
    let fm = ForceMask::default();
    let use_config = UseConfig::new();
    let adapter = Adapter {
        data,
        arch: &arch,
        accept_keywords: &accept,
        package_mask: &[],
        accept_license: &lic,
        use_config: &use_config,
        package_use: &[],
        force_mask: &fm,
        autosolve_use: autosolve,
    };
    let mut provider = PortageDependencyProvider::new(adapter);
    let roots: Vec<_> = targets
        .iter()
        .map(|t| {
            let dep = Dep::parse(t).unwrap();
            let pkg = target_package(data, &dep, &arch, &accept, &[], &lic);
            (pkg, PortageVersionSet::any())
        })
        .collect();
    let sol = provider.resolve_targets(roots).map_err(|e| format!("{e:?}"))?;
    let plan = sol
        .iter()
        .filter(|(p, _)| !p.is_virtual())
        .map(|(p, v)| format!("{}-{}", p.cpn(), v))
        .collect();
    Ok(Outcome { reqs: provider.use_flag_requirements().to_vec(), plan })
}

// md5-cache helpers ---------------------------------------------------------

/// A leaf package with the given IUSE (space-separated, `+` for default-on).
fn leaf(iuse: &str, required_use: &str) -> String {
    let ru = if required_use.is_empty() {
        String::new()
    } else {
        format!("REQUIRED_USE={required_use}\n")
    };
    format!("EAPI=8\nSLOT=0\nIUSE={iuse}\nKEYWORDS=amd64\nDESCRIPTION=t\n{ru}")
}

/// A package whose RDEPEND is `rdepend`, with optional own IUSE.
fn parent(iuse: &str, rdepend: &str) -> String {
    format!("EAPI=8\nSLOT=0\nIUSE={iuse}\nKEYWORDS=amd64\nDESCRIPTION=t\nRDEPEND={rdepend}\n")
}

// ---------------------------------------------------------------------------
// CC1 — plain `[bar]`, target's bar is off: satisfiable by enabling bar.
// ---------------------------------------------------------------------------
#[test]
fn cc1_plain_enabled_dep_detected() {
    let data = repo_from(&[
        ("app/parent-1", &parent("", "dev/foo[bar]")),
        ("dev/foo-1", &leaf("bar", "")), // bar default off
    ]);
    let out = solve(&data, &["app/parent"], false).unwrap();
    assert!(out.has("dev/foo-1"), "foo is pulled into the plan");
    // Tier 2 today: the requirement is detected, bar stays off (suggested only).
    assert!(out.needs_enabled("dev/foo", "bar"), "solver detects foo needs bar");
    // C7 target (autosolve): cede bar on foo, re-solve → no leftover requirement,
    //   the plan carries foo with bar enabled.
}

// ---------------------------------------------------------------------------
// CC2 — plain `[-bar]`, target's bar is on by default: needs disabling.
// ---------------------------------------------------------------------------
#[test]
fn cc2_plain_disabled_dep_detected() {
    let data = repo_from(&[
        ("app/parent-1", &parent("", "dev/foo[-bar]")),
        ("dev/foo-1", &leaf("+bar", "")), // bar default ON
    ]);
    let out = solve(&data, &["app/parent"], false).unwrap();
    assert!(out.needs_disabled("dev/foo", "bar"), "solver detects foo must drop bar");
    // C7 target: cede bar OFF on foo.
}

// ---------------------------------------------------------------------------
// CC3 — conditional `[bar?]`: target needs bar only if the parent has bar.
// ---------------------------------------------------------------------------
#[test]
fn cc3_conditional_dep_detected_when_parent_flag_on() {
    let data = repo_from(&[
        // parent has bar ON, so foo[bar?] requires foo to have bar.
        ("app/parent-1", &parent("+bar", "dev/foo[bar?]")),
        ("dev/foo-1", &leaf("bar", "")),
    ]);
    let out = solve(&data, &["app/parent"], false).unwrap();
    assert!(out.needs_enabled("dev/foo", "bar"), "parent bar on ⇒ foo needs bar");
    // C7 target: cede bar on foo to match the active conditional.
}

// ---------------------------------------------------------------------------
// CC4 — equality `[bar=]`: target's bar must equal the parent's bar.
// ---------------------------------------------------------------------------
#[test]
fn cc4_equal_dep_detected() {
    let data = repo_from(&[
        ("app/parent-1", &parent("+bar", "dev/foo[bar=]")), // parent bar on
        ("dev/foo-1", &leaf("bar", "")),                    // foo bar off ⇒ mismatch
    ]);
    let out = solve(&data, &["app/parent"], false).unwrap();
    assert!(out.needs_enabled("dev/foo", "bar"), "bar= with parent on ⇒ foo needs bar");
    // C7 target: cede foo bar to equal the parent's resolved bar.
}

// ---------------------------------------------------------------------------
// CC5 — `[bar]` collides with the target's REQUIRED_USE (`?? ( bar baz )`,
// baz default-on): satisfying the dep forces a second flag to move.
// ---------------------------------------------------------------------------
#[test]
fn cc5_dep_interacts_with_target_required_use() {
    let data = repo_from(&[
        ("app/parent-1", &parent("", "dev/foo[bar]")),
        ("dev/foo-1", &leaf("bar +baz", "?? ( bar baz )")), // baz on; bar+baz illegal
    ]);
    let out = solve(&data, &["app/parent"], false).unwrap();
    assert!(out.needs_enabled("dev/foo", "bar"), "foo still needs bar");
    // C7 target: ceding bar on must, via Level-C, also drop baz so `?? ( bar baz )`
    //   holds — i.e. C7 and Level-C must co-operate in one re-solve.
}

// ---------------------------------------------------------------------------
// CC6 — unsatisfiable: two parents demand opposite values of the same flag.
// Must stay advisory (no legal cede), never crash.
// ---------------------------------------------------------------------------
#[test]
fn cc6_conflicting_parents_stay_advisory() {
    let data = repo_from(&[
        ("app/p1-1", &parent("", "dev/foo[bar]")),
        ("app/p2-1", &parent("", "dev/foo[-bar]")),
        ("dev/foo-1", &leaf("bar", "")),
    ]);
    let out = solve(&data, &["app/p1", "app/p2"], false).unwrap();
    // Current behaviour is *lossy*: only one side of the contradiction is
    // recorded (the enable, from p1); the `-bar` from p2 is silently dropped.
    assert!(out.needs_enabled("dev/foo", "bar"));
    assert!(
        !out.needs_disabled("dev/foo", "bar"),
        "today the opposite requirement is not surfaced (a pre-existing gap)"
    );
    // C7 target: a co-solve attempt must *detect* the contradiction (both sides)
    //   and leave foo's bar un-ceded — Tier-2 advisory — never loop or crash.
}

// ---------------------------------------------------------------------------
// CC7 — `[bar]` where bar is not in the target's IUSE: cannot be ceded.
// ---------------------------------------------------------------------------
#[test]
fn cc7_flag_absent_from_target_iuse() {
    let data = repo_from(&[
        ("app/parent-1", &parent("", "dev/foo[bar]")),
        ("dev/foo-1", &leaf("other", "")), // no bar IUSE
    ]);
    let out = solve(&data, &["app/parent"], false).unwrap();
    assert!(out.has("dev/foo-1"), "foo still resolves");
    // C7 target: bar is not a real flag on foo → never cede; stays advisory
    //   (an autounmask suggestion the user cannot actually apply).
    let _ = out.needs_enabled("dev/foo", "bar");
}
