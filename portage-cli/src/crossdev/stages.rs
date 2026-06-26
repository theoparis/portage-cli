//! Staged ordered-build plans — the stage1/toolchain-bootstrap problem.
//!
//! A [`StagePlan`] is a curated, *ordered* list of builds that the dependency
//! solver cannot produce on its own, because the steps break a bootstrap
//! chicken-and-egg cycle (a compiler needs a libc; a libc needs a compiler).
//! The same step *sequence* serves two flavours ([`BootstrapKind`]):
//!
//! - **cross** — the toolchain bootstrap into the crossdev prefix
//!   (`/usr/<chost>`), atoms under the `cross-<tuple>` overlay;
//! - **native** — a self-hosting stage1 into `--root` (`CHOST == CBUILD`), plain
//!   `::gentoo` atoms. A native stage1 is the same `glibc ↔ gcc` cycle as a
//!   cross toolchain, broken the same staged way — so the two share one driver
//!   (see `todo/em-root-characterization.md`).
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

/// The flavour of staged toolchain bootstrap: **cross** or **native**. The
/// ordered step sequence ([`toolchain_plan`]) is identical; only how each
/// component is named as an atom differs — cross rewrites the category to
/// `cross-<tuple>`, native keeps the real `::gentoo` category. This is the
/// single typed decision point for "build a toolchain into a fresh root": the
/// cross-vs-native split that the driver previously re-derived at each call
/// site (see `todo/cross-support-self-review.md`).
#[derive(Debug, Clone)]
pub enum BootstrapKind {
    /// Cross-compilation into a `<CTARGET>` sysroot (`CBUILD ≠ CHOST`): atoms
    /// resolve to the `cross-<tuple>` overlay category.
    Cross(CrossTarget),
    /// Native self-hosting stage1 into `--root` (`CBUILD == CHOST`): atoms keep
    /// their real `::gentoo` category. GCC two-stage, glibc, with kernel
    /// headers. (A native LLVM stage1 has the same shape but is not yet wired.)
    Native,
}

impl BootstrapKind {
    /// The category-qualified atom for component `(real_cat, pkg)` in
    /// `::gentoo`. Cross maps every component under `cross-<tuple>`; native uses
    /// the real category verbatim.
    fn atom(&self, real_cat: &str, pkg: &str) -> String {
        match self {
            BootstrapKind::Cross(t) => format!("{}/{pkg}", t.category()),
            BootstrapKind::Native => format!("{real_cat}/{pkg}"),
        }
    }

    /// LLVM/Clang model (target runtimes, no two-stage gcc) vs the GCC
    /// two-stage.
    fn llvm(&self) -> bool {
        match self {
            BootstrapKind::Cross(t) => t.llvm,
            BootstrapKind::Native => false,
        }
    }

    /// Whether the target OS has a kernel (the `sys-kernel/linux-headers` step).
    fn has_kernel(&self) -> bool {
        match self {
            BootstrapKind::Cross(t) => t.has_kernel,
            BootstrapKind::Native => true,
        }
    }

    /// The libc package name (`glibc` / `musl` / `newlib`).
    fn libc_pkg(&self) -> &'static str {
        match self {
            BootstrapKind::Cross(t) => t.libc.pkg_name(),
            BootstrapKind::Native => "glibc",
        }
    }
}

/// The staged toolchain-bootstrap plan for `kind`: the ordered crossdev
/// sequence that produces a working compiler + headers + libc in a fresh root.
/// The driver must run the whole thing — the compiler is not usable until the
/// libc step lands, so the toolchain and the (stage1) libc are one intertwined
/// bootstrap.
pub fn toolchain_plan(kind: &BootstrapKind) -> StagePlan {
    let atom = |real_cat: &str, pkg: &str| kind.atom(real_cat, pkg);
    let owned = |toks: &[&str]| toks.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut steps = Vec::new();

    if kind.llvm() {
        // LLVM model: host clang already cross-targets, so there is no two-stage
        // gcc. Wrappers → kernel headers → libc → runtimes.
        steps.push(StageStep {
            label: "clang wrappers".into(),
            atoms: vec![atom("sys-devel", "clang-crossdev-wrappers")],
            use_override: vec![],
            nodeps: false,
        });
        if kind.has_kernel() {
            steps.push(StageStep {
                label: "kernel headers".into(),
                atoms: vec![atom("sys-kernel", "linux-headers")],
                use_override: owned(&["headers-only"]),
                nodeps: false,
            });
        }
        steps.push(StageStep {
            label: "libc".into(),
            atoms: vec![atom("sys-libs", kind.libc_pkg())],
            use_override: vec![],
            nodeps: false,
        });
        for rt in ["compiler-rt", "libunwind", "libcxxabi", "libcxx"] {
            steps.push(StageStep {
                label: rt.into(),
                atoms: vec![atom("llvm-runtimes", rt)],
                use_override: vec![],
                nodeps: false,
            });
        }
        return StagePlan { steps };
    }

    // GCC model: the classic two-stage bootstrap.
    //
    // Native binutils installs into the (empty) ROOT, so its optional
    // `debuginfod` dep would drag elfutils → curl → … → glibc into this first
    // step, before the staged toolchain exists — 47 packages instead of 7, and
    // the pulled-in glibc then trips the pre-flight on os-headers. Cross binutils
    // is host-rooted (`CBUILD`), so those deps are already satisfied on `/` — it
    // keeps the flag. Disable it only for native; a stage1 doesn't need it.
    let binutils_use = match kind {
        BootstrapKind::Native => owned(&["-debuginfod"]),
        _ => vec![],
    };
    steps.push(StageStep {
        label: "binutils".into(),
        atoms: vec![atom("sys-devel", "binutils")],
        use_override: binutils_use,
        nodeps: false,
    });
    if kind.has_kernel() {
        steps.push(StageStep {
            label: "kernel headers".into(),
            atoms: vec![atom("sys-kernel", "linux-headers")],
            use_override: owned(&["headers-only"]),
            nodeps: false,
        });
        // libc headers first (--nodeps): gcc-stage1 needs them, but glibc itself
        // may DEPEND on a newer gcc we don't have yet — break the cycle.
        steps.push(StageStep {
            label: "libc headers".into(),
            atoms: vec![atom("sys-libs", kind.libc_pkg())],
            use_override: owned(&["headers-only"]),
            nodeps: true,
        });
    }
    let mut stage1 = owned(GCC_DISABLE);
    stage1.extend(owned(GCC_DISABLE_STAGE1));
    steps.push(StageStep {
        label: "gcc-stage1".into(),
        atoms: vec![atom("sys-devel", "gcc")],
        use_override: stage1,
        nodeps: false,
    });
    steps.push(StageStep {
        label: "libc".into(),
        atoms: vec![atom("sys-libs", kind.libc_pkg())],
        use_override: vec![],
        nodeps: false,
    });
    let mut stage2 = owned(GCC_DISABLE);
    stage2.extend(owned(GCC_DISABLE_STAGE2));
    steps.push(StageStep {
        label: "gcc-stage2".into(),
        atoms: vec![atom("sys-devel", "gcc")],
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
        let plan = toolchain_plan(&BootstrapKind::Cross(t));
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
        let plan = toolchain_plan(&BootstrapKind::Cross(t));
        assert!(!labels(&plan).contains(&"kernel headers"));
        assert!(plan.steps.iter().any(|s| s.atoms[0].ends_with("/newlib")));
    }

    #[test]
    fn llvm_plan_has_runtimes_not_two_stage_gcc() {
        let t = CrossTarget::parse("aarch64-unknown-linux-musl", true).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t));
        let l = labels(&plan);
        assert!(l.contains(&"clang wrappers"));
        assert!(l.contains(&"compiler-rt"));
        assert!(!l.iter().any(|s| s.starts_with("gcc-stage")));
        assert!(plan.steps.iter().any(|s| s.atoms[0].ends_with("/musl")));
    }

    #[test]
    fn native_plan_uses_real_categories_and_is_gcc_glibc() {
        // A native stage1 (CHOST == CBUILD) uses plain ::gentoo atoms: no
        // cross-* overlay category. Same GCC two-stage + glibc sequence as cross.
        let plan = toolchain_plan(&BootstrapKind::Native);
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
        // Real categories, no `cross-` rewrite.
        let atoms: Vec<&str> = plan
            .steps
            .iter()
            .flat_map(|s| s.atoms.iter().map(|a| a.as_str()))
            .collect();
        assert_eq!(atoms[0], "sys-devel/binutils");
        assert_eq!(atoms[1], "sys-kernel/linux-headers");
        assert!(atoms.iter().all(|a| !a.starts_with("cross-")));
        // libc is glibc, in sys-libs.
        assert_eq!(plan.steps[2].atoms[0], "sys-libs/glibc");
        assert_eq!(plan.steps[4].atoms[0], "sys-libs/glibc");
        // The staged USE overrides still apply (stage1 drops cxx).
        assert!(plan.steps[3].use_override.contains(&"-cxx".to_string()));
        assert!(!plan.steps[5].use_override.contains(&"-cxx".to_string()));
        // The libc-headers cycle-breaker is still --nodeps + headers-only.
        assert!(plan.steps[2].nodeps);
        assert!(
            plan.steps[2]
                .use_override
                .contains(&"headers-only".to_string())
        );
        // Native binutils drops debuginfod (else its elfutils→…→glibc closure
        // explodes step 1 into the empty ROOT).
        assert!(
            plan.steps[0]
                .use_override
                .contains(&"-debuginfod".to_string())
        );
    }

    #[test]
    fn cross_binutils_keeps_debuginfod() {
        // Cross binutils is host-rooted, so its debuginfod deps are
        // host-satisfied — no need to force the flag off (behaviour-preserving).
        let t = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        let plan = toolchain_plan(&BootstrapKind::Cross(t));
        assert_eq!(plan.steps[0].label, "binutils");
        assert!(plan.steps[0].use_override.is_empty());
    }
}
