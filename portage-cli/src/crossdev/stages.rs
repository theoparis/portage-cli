//! Staged ordered-build plans for the cross environment — the stage1/stage3
//! problem.
//!
//! A [`StagePlan`] is a curated, *ordered* list of builds that the dependency
//! solver cannot produce on its own, because the steps break a bootstrap
//! chicken-and-egg cycle (a compiler needs a libc; a libc needs a compiler).
//! The first use is the **toolchain bootstrap** into the crossdev prefix
//! (`/usr/<chost>`): binutils → kernel/libc headers → gcc-stage1 → libc →
//! gcc-stage2. The same driver will later run the **target stage** (stage1/
//! stage3 rootfs) plan via `--cross`.
//!
//! Each step is one `em`-equivalent merge with a per-step USE override and the
//! `--nodeps` / `headers-only` bootstrap flags crossdev uses (`/usr/bin/crossdev`
//! `doemerge` loop). em owns only the ordering + USE/flags here; the
//! stage1-vs-stage2 gcc *behaviour* is auto-detected by `toolchain.eclass` from
//! whether the libc is present in the prefix yet, exactly as under crossdev.

use super::target::{CrossTarget, Libc};

/// gcc USE forced off for **every** cross gcc build (crossdev `GUSE_DISABLE`).
const GCC_DISABLE: &[&str] = &["-objc", "-objc++", "-objc-gc", "-vtv"];
/// Additional gcc USE forced off for **stage1** — a freestanding C compiler with
/// no libc yet (crossdev `GUSE_DISABLE_STAGE_1`).
const GCC_DISABLE_STAGE1: &[&str] = &[
    "-fortran",
    "-d",
    "-go",
    "-jit",
    "-cxx",
    "-openmp",
    "-sanitize",
    "-zstd",
    "-zlib",
];
/// Additional gcc USE forced off for **stage2** (crossdev `GUSE_DISABLE_STAGE_2`).
const GCC_DISABLE_STAGE2: &[&str] = &["-sanitize"];

/// One ordered build in a [`StagePlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageStep {
    /// Human label for progress / dry-run (e.g. `gcc-stage1`).
    pub label: String,
    /// Atoms to merge for this step, in order (e.g. `cross-riscv64-…/gcc`).
    pub atoms: Vec<String>,
    /// USE tokens forced for this step, in emerge syntax (`headers-only`,
    /// `-cxx`). Applied on top of the configured USE.
    pub use_override: Vec<String>,
    /// Skip dependency resolution (crossdev's `--nodeps`): used for the
    /// headers-only libc step, to break the glibc→newer-gcc cycle before a
    /// compiler exists.
    pub nodeps: bool,
}

/// An ordered sequence of [`StageStep`]s run against one root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePlan {
    /// The steps, in build order.
    pub steps: Vec<StageStep>,
}

impl Libc {
    /// Package name in `::gentoo` (the `cross-*` overlay symlinks the same name).
    fn pkg_name(self) -> &'static str {
        match self {
            Libc::Glibc => "glibc",
            Libc::Musl => "musl",
            Libc::Newlib => "newlib",
        }
    }
}

/// The toolchain bootstrap plan for `target`: the ordered crossdev sequence that
/// produces a working `/usr/<chost>` (compiler + headers + libc). `--setup` must
/// run the whole thing — the compiler is not usable until the libc step lands,
/// so the toolchain and the stage1 libc are one intertwined bootstrap.
pub fn toolchain_plan(target: &CrossTarget) -> StagePlan {
    let cat = target.category();
    let atom = |pkg: &str| format!("{cat}/{pkg}");
    let owned = |toks: &[&str]| toks.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut steps = Vec::new();

    if target.llvm {
        // LLVM model: host clang already cross-targets, so there is no two-stage
        // gcc. Wrappers → kernel headers → libc → runtimes.
        steps.push(StageStep {
            label: "clang wrappers".into(),
            atoms: vec![atom("clang-crossdev-wrappers")],
            use_override: vec![],
            nodeps: false,
        });
        if target.has_kernel {
            steps.push(StageStep {
                label: "kernel headers".into(),
                atoms: vec![atom("linux-headers")],
                use_override: owned(&["headers-only"]),
                nodeps: false,
            });
        }
        steps.push(StageStep {
            label: "libc".into(),
            atoms: vec![atom(target.libc.pkg_name())],
            use_override: vec![],
            nodeps: false,
        });
        for rt in ["compiler-rt", "libunwind", "libcxxabi", "libcxx"] {
            steps.push(StageStep {
                label: rt.into(),
                atoms: vec![atom(rt)],
                use_override: vec![],
                nodeps: false,
            });
        }
        return StagePlan { steps };
    }

    // GCC model: the classic two-stage bootstrap.
    steps.push(StageStep {
        label: "binutils".into(),
        atoms: vec![atom("binutils")],
        use_override: vec![],
        nodeps: false,
    });
    if target.has_kernel {
        steps.push(StageStep {
            label: "kernel headers".into(),
            atoms: vec![atom("linux-headers")],
            use_override: owned(&["headers-only"]),
            nodeps: false,
        });
        // libc headers first (--nodeps): gcc-stage1 needs them, but glibc itself
        // may DEPEND on a newer gcc we don't have yet — break the cycle.
        steps.push(StageStep {
            label: "libc headers".into(),
            atoms: vec![atom(target.libc.pkg_name())],
            use_override: owned(&["headers-only"]),
            nodeps: true,
        });
    }
    let mut stage1 = owned(GCC_DISABLE);
    stage1.extend(owned(GCC_DISABLE_STAGE1));
    steps.push(StageStep {
        label: "gcc-stage1".into(),
        atoms: vec![atom("gcc")],
        use_override: stage1,
        nodeps: false,
    });
    steps.push(StageStep {
        label: "libc".into(),
        atoms: vec![atom(target.libc.pkg_name())],
        use_override: vec![],
        nodeps: false,
    });
    let mut stage2 = owned(GCC_DISABLE);
    stage2.extend(owned(GCC_DISABLE_STAGE2));
    steps.push(StageStep {
        label: "gcc-stage2".into(),
        atoms: vec![atom("gcc")],
        use_override: stage2,
        nodeps: false,
    });
    StagePlan { steps }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(plan: &StagePlan) -> Vec<&str> {
        plan.steps.iter().map(|s| s.label.as_str()).collect()
    }

    #[test]
    fn gcc_glibc_plan_is_the_two_stage_bootstrap() {
        let t = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let plan = toolchain_plan(&t);
        assert_eq!(
            labels(&plan),
            [
                "binutils",
                "kernel headers",
                "libc headers",
                "gcc-stage1",
                "libc",
                "gcc-stage2",
            ]
        );
        // Atoms live in the cross-* overlay category.
        assert!(
            plan.steps[0].atoms[0].starts_with("cross-riscv64-unknown-linux-gnu/"),
            "{:?}",
            plan.steps[0].atoms
        );
        // libc headers step is the --nodeps cycle-breaker.
        let libc_headers = &plan.steps[2];
        assert!(libc_headers.nodeps);
        assert!(
            libc_headers
                .use_override
                .contains(&"headers-only".to_string())
        );
        // stage1 gcc drops cxx/libc-dependent USE; stage2 keeps them.
        assert!(plan.steps[3].use_override.contains(&"-cxx".to_string()));
        assert!(!plan.steps[5].use_override.contains(&"-cxx".to_string()));
    }

    #[test]
    fn baremetal_newlib_has_no_kernel_headers() {
        let t = CrossTarget::parse("riscv64-unknown-elf", false).unwrap();
        let plan = toolchain_plan(&t);
        assert!(!labels(&plan).contains(&"kernel headers"));
        assert!(plan.steps.iter().any(|s| s.atoms[0].ends_with("/newlib")));
    }

    #[test]
    fn llvm_plan_has_runtimes_not_two_stage_gcc() {
        let t = CrossTarget::parse("aarch64-unknown-linux-musl", true).unwrap();
        let plan = toolchain_plan(&t);
        let l = labels(&plan);
        assert!(l.contains(&"clang wrappers"));
        assert!(l.contains(&"compiler-rt"));
        assert!(!l.iter().any(|s| s.starts_with("gcc-stage")));
        assert!(plan.steps.iter().any(|s| s.atoms[0].ends_with("/musl")));
    }
}
