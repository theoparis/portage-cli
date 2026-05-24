//! Example: demonstrate dependency resolution **failure modes** with resolvo.
//!
//! Each scenario builds a tiny repository, attempts to solve, and prints
//! the human-readable conflict explanation produced by
//! `Conflict::display_user_friendly`.

use std::collections::HashSet;

use portage_atom::{Cpv, Dep};
use portage_atom_resolvo::{
    DepEntry, InMemoryRepository, InstalledPolicy, InstalledSet, PackageDeps, PackageMetadata,
    PortageDependencyProvider, UseConfig,
};
use resolvo::{Problem, Solver, UnsolvableOrCancelled};

/// Shorthand to build a [`PackageMetadata`] from a CPV string.
fn pkg(cpv: &str, slot: &str, deps: Vec<DepEntry>) -> PackageMetadata {
    PackageMetadata {
        cpv: Cpv::parse(cpv).unwrap(),
        slot: Some(slot.into()),
        subslot: None,
        iuse: vec![],
        use_flags: HashSet::new(),
        repo: None,
        dependencies: PackageDeps {
            depend: deps,
            ..PackageDeps::default()
        },
    }
}

/// Build a provider, solve, and print the result (expected to fail).
fn try_solve(title: &str, repo: &InMemoryRepository, use_config: &UseConfig, root_atoms: &[&str]) {
    println!("\n{}", "=".repeat(60));
    println!("{title}");
    println!("{}", "=".repeat(60));

    let mut provider = PortageDependencyProvider::new(repo, use_config);

    let reqs: Vec<_> = root_atoms
        .iter()
        .map(|s| provider.intern_requirement(&Dep::parse(s).unwrap()))
        .collect();
    let problem = Problem::new().requirements(reqs);

    let mut solver = Solver::new(provider);
    match solver.solve(problem) {
        Ok(_solution) => {
            println!("  Resolved successfully (unexpected for this example).");
        }
        Err(UnsolvableOrCancelled::Unsolvable(conflict)) => {
            println!("{}", conflict.display_user_friendly(&solver));
        }
        Err(UnsolvableOrCancelled::Cancelled(_)) => {
            println!("  Cancelled.");
        }
    }
}

/// Variant of [`try_solve`] that accepts an installed set.
fn try_solve_with_installed(
    title: &str,
    repo: &InMemoryRepository,
    use_config: &UseConfig,
    installed: &InstalledSet,
    root_atoms: &[&str],
) {
    println!("\n{}", "=".repeat(60));
    println!("{title}");
    println!("{}", "=".repeat(60));

    let mut provider = PortageDependencyProvider::with_installed(repo, use_config, installed);

    let reqs: Vec<_> = root_atoms
        .iter()
        .map(|s| provider.intern_requirement(&Dep::parse(s).unwrap()))
        .collect();
    let problem = Problem::new().requirements(reqs);

    let mut solver = Solver::new(provider);
    match solver.solve(problem) {
        Ok(_solution) => {
            println!("  Resolved successfully (unexpected for this example).");
        }
        Err(UnsolvableOrCancelled::Unsolvable(conflict)) => {
            println!("{}", conflict.display_user_friendly(&solver));
        }
        Err(UnsolvableOrCancelled::Cancelled(_)) => {
            println!("  Cancelled.");
        }
    }
}

fn main() {
    let use_config = UseConfig::default();

    // ── 1. Missing dependency ─────────────────────────────────────────
    {
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "app-misc/hello-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lib/nonexistent").unwrap())],
        ));
        try_solve(
            "1. Missing dependency — no candidates at all",
            &repo,
            &use_config,
            &["app-misc/hello"],
        );
    }

    // ── 2. Version conflict ───────────────────────────────────────────
    {
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lib/foo-1.0", "0", vec![]));
        repo.add(pkg(
            "app-misc/myapp-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse(">=dev-lib/foo-2.0").unwrap())],
        ));
        try_solve(
            "2. Version conflict — needs >=2.0, only 1.0 exists",
            &repo,
            &use_config,
            &["app-misc/myapp"],
        );
    }

    // ── 3. Mutual blockers ────────────────────────────────────────────
    {
        let mut repo = InMemoryRepository::new();
        repo.add(pkg(
            "dev-libs/openssl-3.2.1",
            "0",
            vec![DepEntry::Atom(Dep::parse("!dev-libs/libressl").unwrap())],
        ));
        repo.add(pkg(
            "dev-libs/libressl-3.9.2",
            "0",
            vec![DepEntry::Atom(Dep::parse("!!dev-libs/openssl").unwrap())],
        ));
        repo.add(pkg(
            "app-misc/myapp-1.0",
            "0",
            vec![
                DepEntry::Atom(Dep::parse("dev-libs/openssl").unwrap()),
                DepEntry::Atom(Dep::parse("dev-libs/libressl").unwrap()),
            ],
        ));
        try_solve(
            "3. Mutual blockers — app requires both openssl and libressl",
            &repo,
            &use_config,
            &["app-misc/myapp"],
        );
    }

    // ── 4. Locked package conflict ────────────────────────────────────
    {
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lib/bar-1.0", "0", vec![]));
        repo.add(pkg("dev-lib/bar-2.0", "0", vec![]));
        repo.add(pkg(
            "app-misc/myapp-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse(">=dev-lib/bar-2.0").unwrap())],
        ));

        let mut installed = InstalledSet::new();
        installed.add(pkg("dev-lib/bar-1.0", "0", vec![]), InstalledPolicy::Locked);

        try_solve_with_installed(
            "4. Locked package conflict — bar-1.0 locked, app needs >=2.0",
            &repo,
            &use_config,
            &installed,
            &["app-misc/myapp"],
        );
    }

    // ── 5. Slot conflict ──────────────────────────────────────────────
    {
        let mut repo = InMemoryRepository::new();
        repo.add(pkg("dev-lang/python-3.12.4", "3.12", vec![]));
        repo.add(pkg(
            "app-misc/myapp-1.0",
            "0",
            vec![DepEntry::Atom(Dep::parse("dev-lang/python:3.11").unwrap())],
        ));
        try_solve(
            "5. Slot conflict — needs python:3.11, only :3.12 available",
            &repo,
            &use_config,
            &["app-misc/myapp"],
        );
    }
}
