//! Parse a crossdev target tuple (`ARCH-VENDOR-OS-LIBC`) and derive everything
//! the no-build setup needs: the overlay category, the package set to symlink,
//! the Gentoo `ARCH`/keyword, the profile path, and the target `CFLAGS`.
//!
//! This mirrors crossdev's `parse_target` + the package-class table
//! (`/usr/bin/crossdev`, `BCAT/GCAT/KCAT/LCAT/...`) and crossdev-stages'
//! `gentoo_arch`/`gentoo_profile`/`target_cflags` (`lib/common.sh`), reduced to
//! the libc models em supports today: glibc (`gnu`), musl, and newlib
//! (bare-metal `-elf`/`-eabi`).

use anyhow::{Result, bail};
use gentoo_core::Arch;

/// The target C library, chosen from the tuple's last field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Libc {
    /// `…-linux-gnu` — `sys-libs/glibc`.
    Glibc,
    /// `…-linux-musl` — `sys-libs/musl`.
    Musl,
    /// `…-elf`/`-eabi`/`-newlib` — `sys-libs/newlib`, bare metal (no kernel).
    Newlib,
}

impl Libc {
    /// The real `category/package` providing this libc in `::gentoo`.
    fn package(self) -> (&'static str, &'static str) {
        match self {
            Libc::Glibc => ("sys-libs", "glibc"),
            Libc::Musl => ("sys-libs", "musl"),
            Libc::Newlib => ("sys-libs", "newlib"),
        }
    }
}

/// A parsed cross target plus the toolchain model (`--llvm`).
#[derive(Debug, Clone)]
pub struct CrossTarget {
    /// The full `CTARGET` tuple, e.g. `riscv64-unknown-linux-gnu`.
    pub tuple: String,
    /// The CPU field (`tuple` before the first `-`), e.g. `riscv64`.
    pub cpu: String,
    /// The target libc.
    pub libc: Libc,
    /// Whether the OS has a kernel (`linux`) — bare-metal targets do not, so they
    /// skip `sys-kernel/linux-headers`.
    pub has_kernel: bool,
    /// LLVM/Clang model (`cross_llvm-*`, no per-target compiler) vs GCC.
    pub llvm: bool,
}

impl CrossTarget {
    /// Parse `tuple` (`ARCH-VENDOR-OS-LIBC`); `llvm` selects the Clang model.
    pub fn parse(tuple: &str, llvm: bool) -> Result<Self> {
        let cpu = tuple
            .split('-')
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("empty target tuple"))?;

        // libc/OS from the tuple suffix (crossdev `parse_target`, abbreviated).
        let (libc, has_kernel) = if tuple.ends_with("gnu")
            || tuple.ends_with("gnueabi")
            || tuple.ends_with("gnueabihf")
        {
            (Libc::Glibc, true)
        } else if tuple.ends_with("musl") {
            (Libc::Musl, true)
        } else if tuple.ends_with("elf") || tuple.ends_with("eabi") || tuple.ends_with("newlib") {
            (Libc::Newlib, false)
        } else {
            bail!(
                "unsupported target '{tuple}': em crossdev handles gnu (glibc), \
                 musl, and bare-metal -elf/-eabi (newlib) tuples"
            );
        };

        // crossdev rejects glibc under LLVM ("cannot currently compile glibc").
        if llvm && libc == Libc::Glibc {
            bail!(
                "LLVM/Clang cannot build glibc — use a musl (…-linux-musl) or \
                 bare-metal (…-elf) target with -L, or drop -L for the GCC model"
            );
        }

        Ok(Self {
            tuple: tuple.to_owned(),
            cpu,
            libc,
            has_kernel,
            llvm,
        })
    }

    /// The overlay category for this target: `cross_llvm-<tuple>` (LLVM) or
    /// `cross-<tuple>` (GCC).
    pub fn category(&self) -> String {
        let prefix = if self.llvm { "cross_llvm-" } else { "cross-" };
        format!("{prefix}{}", self.tuple)
    }

    /// The Gentoo `ARCH`/keyword for the target CPU (e.g. `riscv64` → `riscv`).
    pub fn gentoo_arch(&self) -> String {
        Arch::from_chost(&self.tuple)
            .map(|a| a.as_keyword().to_owned())
            .unwrap_or_else(|| self.cpu.clone())
    }

    /// The repo-relative target profile path (`gentoo_profile` in
    /// crossdev-stages). Linked **directly** — `eselect profile` rejects a
    /// foreign arch.
    pub fn profile_path(&self) -> String {
        // Bare-metal (newlib, no kernel): the arch-neutral `embedded` base
        // profile. The arch-specific `default/linux/*` profiles force
        // `kernel_linux` and assume a full OS the target does not have.
        if !self.has_kernel {
            return "embedded".to_owned();
        }
        match self.gentoo_arch().as_str() {
            "riscv" => "default/linux/riscv/23.0/rv64/lp64d".to_owned(),
            "x86" => "default/linux/x86/23.0/i686".to_owned(),
            arch => format!("default/linux/{arch}/23.0"),
        }
    }

    /// Target `CFLAGS` (`target_cflags` in crossdev-stages).
    pub fn cflags(&self) -> &'static str {
        match self.cpu.as_str() {
            "x86_64" => "-O3 -march=x86-64 -pipe",
            "aarch64" => "-O3 -pipe",
            "riscv64" => "-O3 -march=rv64gc -pipe",
            _ => "-O2 -pipe",
        }
    }

    /// The `(real_category, package)` set to symlink into the overlay category,
    /// in stage order. The cross magic lives in the eclasses, triggered by the
    /// `cross-*` category, so these point at the ordinary `::gentoo` ebuilds.
    pub fn packages(&self) -> Vec<(&'static str, &'static str)> {
        let mut pkgs: Vec<(&'static str, &'static str)> = Vec::new();
        if self.llvm {
            // Clang already cross-targets: no per-target compiler, just the
            // wrapper + the target runtimes built into the sysroot.
            pkgs.push(("sys-devel", "clang-crossdev-wrappers"));
            if self.has_kernel {
                pkgs.push(("sys-kernel", "linux-headers"));
            }
            pkgs.push(self.libc.package());
            pkgs.push(("llvm-runtimes", "compiler-rt"));
            pkgs.push(("llvm-runtimes", "libunwind"));
            pkgs.push(("llvm-runtimes", "libcxxabi"));
            pkgs.push(("llvm-runtimes", "libcxx"));
        } else {
            // GCC: the classic binutils → headers → gcc → libc toolchain.
            pkgs.push(("sys-devel", "binutils"));
            if self.has_kernel {
                pkgs.push(("sys-kernel", "linux-headers"));
            }
            pkgs.push(("sys-devel", "gcc"));
            pkgs.push(self.libc.package());
            pkgs.push(("dev-debug", "gdb"));
        }
        pkgs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn riscv_gnu_is_glibc_with_kernel() {
        let t = CrossTarget::parse("riscv64-unknown-linux-gnu", false).unwrap();
        assert_eq!(t.cpu, "riscv64");
        assert_eq!(t.libc, Libc::Glibc);
        assert!(t.has_kernel);
        assert_eq!(t.category(), "cross-riscv64-unknown-linux-gnu");
        assert_eq!(t.gentoo_arch(), "riscv");
        assert_eq!(t.profile_path(), "default/linux/riscv/23.0/rv64/lp64d");
        // binutils, linux-headers, gcc, glibc, gdb
        assert!(t.packages().contains(&("sys-libs", "glibc")));
        assert!(t.packages().contains(&("sys-kernel", "linux-headers")));
    }

    #[test]
    fn baremetal_elf_is_newlib_no_kernel() {
        let t = CrossTarget::parse("riscv64-unknown-elf", false).unwrap();
        assert_eq!(t.libc, Libc::Newlib);
        assert!(!t.has_kernel);
        assert!(!t.packages().contains(&("sys-kernel", "linux-headers")));
        assert!(t.packages().contains(&("sys-libs", "newlib")));
        // bare metal uses the arch-neutral embedded profile, not a linux one
        assert_eq!(t.profile_path(), "embedded");
    }

    #[test]
    fn llvm_uses_cross_llvm_category_and_runtimes() {
        let t = CrossTarget::parse("aarch64-unknown-linux-musl", true).unwrap();
        assert_eq!(t.category(), "cross_llvm-aarch64-unknown-linux-musl");
        assert!(
            t.packages()
                .contains(&("sys-devel", "clang-crossdev-wrappers"))
        );
        assert!(t.packages().contains(&("llvm-runtimes", "compiler-rt")));
        // no per-target gcc/binutils
        assert!(!t.packages().contains(&("sys-devel", "gcc")));
    }

    #[test]
    fn llvm_rejects_glibc() {
        let err = CrossTarget::parse("riscv64-unknown-linux-gnu", true).unwrap_err();
        assert!(err.to_string().contains("glibc"));
    }
}
