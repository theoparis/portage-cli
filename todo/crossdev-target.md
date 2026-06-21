# Crossdev `{target}` — build a cross sysroot + compiler(s)

STATUS: **planning / not started beyond `-p` parity.** Goal: make `em` act as a
`{target}-emerge` that actually *builds* a cross toolchain and sysroot for a
foreign `CHOST` (`CBUILD ≠ CHOST`), covering **both** the GCC and the
**LLVM/Clang** toolchain models. Target libc is the standard choice (glibc /
musl, and LLVM libc only as a generic option if a target wants it).

Authoritative design context: `docs/root-model.md` (§ Cross, § Sequencing),
`todo/em-root-characterization.md`, `todo/nonemptytree-bdeps-gap.md`.

## Host facts (this dev box, arm64)

- Overlays (`/etc/portage/repos.conf`): `gentoo`, `crossdev`
  (`/var/db/repos/crossdev`).
- Cross sysroots laid down: `/usr/riscv64-unknown-linux-gnu`,
  `/usr/aarch64-unknown-linux-gnu`. Installed GCC cross toolchains:
  `cross-riscv64-unknown-{elf,linux-gnu}` (VDB).
- LLVM: `llvm-core/clang` 20/21/22 + `clang-common`, `clang-toolchain-symlinks`,
  `clang-linker-config` installed.

## Already built (the foundation — do not redo)

- **Multi-repo**: the crossdev overlay loads; `cross-*` categories resolve.
- **force/mask for `cross-*`**: `package.use.force/mask/cross-*`
  (multilib/cet/nopie) apply per package.
- **ROOT-offset BDEPEND + Tier-1 host scheduling**: `host_copies.rs` post-solve
  walk emits `MergeRoot::Host` build-copies; offset `@system` = 180 == emerge
  180, `curl` = 15 == 15.
- **Cross `-p` parity**: `riscv64-…-emerge -p gcc` = 18 pkgs already matches.

So **resolution/pretend for cross is essentially done**; everything below is past
`-p`.

## Prerequisites — CONFIRMED LIVE (2026-06-20, release `em`)

Verified against the actual binary/code, not docs (which understated it):

| # | Prerequisite | Check | Result |
|---|---|---|---|
| P1 | crossdev overlay + `cross-*` resolve (symlink-follow) | `em -p cross-riscv64-unknown-linux-gnu/binutils` | `U 2.46.1 [2.46.0]` ✓ |
| P2 | `--root` target-root offset | `em -p --root /tmp/e --config-root / zlib` | `N … to /tmp/e/` ✓ |
| P3 | `--prefix` offset | `em -p --prefix /tmp/p zlib` | `R … to /tmp/p/` ✓ |
| P4 | `--local` `EPREFIX=~/.gentoo` (the **retarget** primitive) | `em -p --local zlib` | `R … to ~/.gentoo/` ✓ |
| P5 | `MergeRoot::Host` build-closure walk wired | `host_copies::compute(...,&cross)` spliced into plan (mod.rs:636) | ✓ |
| P6 | `--setup` prefix bootstrap | `setup.rs::bootstrap` (needs `--local`/`--prefix`) | ✓ |
| P7 | root derivation composes config/base/target/eprefix | `Cli::roots()` (cli.rs) | ✓ |
| P8 | **cross-context detection + dual-root routing already exist** | `root_aware::CrossContext{sysroot,target,chost,cbuild,active}`, `detect()` reads `CHOST`/`CBUILD` from the sysroot make.conf, `is_cross_arch()`; `MergeRoot` nodes in the solver | ✓ |

**Reconciliation:** the earlier "crossdev not started beyond `-p`" framing was
stale. Cross-context **detection** and **dual-root `MergeRoot` routing** are
implemented (`root_aware.rs`, `host_copies.rs`, pubgrub `MergeRoot`). What's
genuinely missing: (a) the **entry-point ergonomics** — a `--cross <tuple>` /
`<CTARGET>-emerge` that points `config_root` at the sysroot (today hand-driven as
`--config-root /usr/<CTARGET> --root /usr/<CTARGET>`); (b) the actual cross
**build** (run phases with cross env); (c) the **`--local` sub-path retarget**
(below).

### Gap for the retarget requirement (point 1)
`--local` currently hardcodes `target = EPREFIX = ~/.gentoo`. A cross sysroot at
`~/.gentoo/usr/<CTARGET>` needs either `--prefix ~/.gentoo/usr/<CTARGET>` (works
today, but config comes from the host, not the sysroot) or — better — the cross
entry point (concern 2) setting `EPREFIX=~/.gentoo`, `sysroot = target =
$EPREFIX/usr/<CTARGET>`, and `config_root` at that sysroot. The primitives (P3/P4/
P7) compose; only the cross-specific wiring is missing.

## Decomposition: three orthogonal concerns (the planning frame)

crossdev conflates three things into one stage loop. em should treat them as
**separate concerns split by install root**, each mapping to an existing em
primitive:

### 1. Populate the `<EPREFIX>/usr/<CTARGET>` sysroot — *Target root, ≈ `--local --setup`*
The target-arch artifacts: kernel-headers, libc (glibc/musl), and the runtimes
(compiler-rt, libunwind, libc++/libc++abi). They install **into the sysroot**
(cross-built). This is em's ROOT-offset build of target packages — the
`--root`/`--local` machinery — plus a `--setup`-style bootstrap to lay down the
empty sysroot's base config (crossdev writes
`<sysroot>/etc/portage/{make.conf,package.*}`; em's `--local --setup` already
bootstraps a prefix — reuse that path, see `setup.rs`).

**REQUIREMENT — the sysroot must be RELOCATABLE, not hardcoded to `/usr/<CTARGET>`.**
The axis is **self-contained (own libc/kernel) vs host-shared**, and it is the
`--root` vs `--prefix` distinction (NOT `--local`, which is merely a prefix):

| mode | sysroot location | libc + kernel | which em primitive |
|---|---|---|---|
| **default** `em crossdev <t>` | `/usr/<CTARGET>` (`EPREFIX=/`) | system | (root install, crossdev parity) |
| **`--root DIR`** | `DIR` (own VDB) | **built from scratch** (self-contained) → the "stage1 from scratch" | `--root` offset, empty VDB ⇒ full closure `N` |
| **`--prefix DIR`** (and `--local` = `--prefix ~/.gentoo`) | `DIR` | **host's libc+kernel SHARED** (base = host; only the delta builds) | prefix overlay, host VDB shared ⇒ delta only |

- **`--local` is shorthand for `--prefix ~/.gentoo`** — a host-sharing prefix,
  *not* self-contained. (Smoke test confirmed: `em -p --local cross-…/gcc` →
  `U gcc`, no closure, because the host's cross binutils/glibc/headers are
  shared. Correct for a prefix.)
- **`--root <empty>`** is the self-contained path: an isolated VDB ⇒ em plans the
  whole toolchain closure from scratch (all `N`), own libc/kernel. This is "stage1
  from scratch".
- **`--prefix`** = root-model `target ≠ base` (base = host): share host
  libc+kernel-headers, build only what's missing — much lighter.

Both must work; the driver (concern 2) takes the sysroot/prefix/root as input and
nothing may assume `/usr/<CTARGET>`.

This reuses the `--local`/`--prefix` EPREFIX machinery (`root-model.md`,
[[local-eprefix-mode]]): every cross location var gets the prefix —
`SYSROOT=ESYSROOT=<EPREFIX>/usr/<CTARGET>`, `ROOT` likewise, and
`PORTAGE_CONFIGROOT=<EPREFIX>/usr/<CTARGET>`. NB for LLVM:
the generated `clang` cross cfg (`--sysroot=…`) and `/etc/clang/cross/*.cfg`
location must also follow `<EPREFIX>`, not the hardcoded `/usr/<CTARGET>` crossdev
writes.

### 2. The driver — *a dedicated `em crossdev` subcommand (+ a `<CTARGET>-emerge` wrapper)*
**Make it a separate subcommand**, not just a flag, so users coming from the
original `crossdev` get a seamless/familiar interface. Two entry forms:

- **`em crossdev -t <tuple> [-s0..s4] [-L] [--ex-pkg X] [--ov-output DIR] …`** —
  the orchestrator, **mirroring the original `crossdev` option surface** (same
  flags: `-t/--target`, the stage flags, `-L/--llvm`, `--ex-*`, the overlay/`--ov-*`
  and package-override `--[bdgkl]pkg/cat/env` options, `-S/--stable`, `-C/--clean`,
  `--init-target`, `--show-target-cfg`). It does concern-1 init/setup (lay down
  the sysroot base + overlay, reusing `--setup`/`setup.rs`) and drives the stage
  sequence (concerns 1+3) over em's own resolve+build — replacing the crossdev
  bash, not shelling out to it.
  **Default install target: `/usr/<CTARGET>`** (`EPREFIX=/`), exactly like the
  original crossdev — bare `em crossdev <tuple>` is the privileged system install
  to `/usr/<CTARGET>`. The retarget (concern 1, `<EPREFIX>/usr/<CTARGET>`, e.g.
  `~/.gentoo/usr/<CTARGET>`) is **opt-in** via `--local`/`--prefix`; the default
  is unchanged so existing crossdev users get identical behaviour.
- **`<CTARGET>-emerge <pkg>`** — the per-target emerge for ongoing builds /
  `--ex-pkg` (concern 3) and target packages (concern 1). Generated by
  `em crossdev` (like crossdev installs `/usr/bin/<CTARGET>-emerge`); it's just
  `em` with the cross context auto-detected (P8 `root_aware::detect` already does
  this from the sysroot make.conf) — sets
  `CHOST/CBUILD, SYSROOT=ESYSROOT=<EPREFIX>/usr/<CTARGET>, BROOT=/, ROOT=…`.

So the subcommand owns the orchestration + UX; the wrapper is the thin
per-package path. Map crossdev flags 1:1 where sensible; document any deltas.

### 3. Host-installed cross tooling — *Host root, the "cross compilers" / `--ex-pkg`*
Things that install on the **host** (`ROOT=/`) but provide target capability:
`cross-<CTARGET>/{binutils,gcc}` (the `<CTARGET>-gcc` binaries), the rust target
std (`rust-std` / `RUST_TARGETS`), `clang-crossdev-wrapper`. Host builds of
`cross-*` packages → `MergeRoot::Host` (the `host_copies.rs` machinery); the
eclass builds them as cross-compilers targeting `<CTARGET>`. `--ex-pkg` just adds
more of these on demand.

**The stage loop is the dependency ordering that interleaves 1 and 3**
(binutils→gcc1 [host] → headers→libc [sysroot] → gcc2 [host] → runtimes
[sysroot]). em's resolver orders by deps; a thin stage driver supplies the
per-stage `USE`.

**LLVM (`-L`) collapses concern 3**: clang already cross-targets, so there is no
per-target compiler to build host-side — concern 3 shrinks to
`clang-crossdev-wrapper` (+ `/etc/clang/cross/<CTARGET>.cfg`), and the bulk is
concern 1 (sysroot) + concern 2 (driver). This is why LLVM leads.

The Stages A–D below are the *implementation* increments; concerns 1/2/3 are the
*architecture* they serve (2 = Stage A; 1 = a Target-root build reusing
`--local`; 3 = Host-root builds via the existing `MergeRoot::Host` walk).

## crossdev-bash behaviour, characterized (2026-06-20, `--show-target-cfg`)

Tuple = `ARCH-VENDOR-OS-LIBC`; libc ∈ `gnu`(glibc)/`musl`/`newlib`(bare metal)/
`uclibc`/`klibc`. `--show-target-cfg -t <tuple>` (safe, no writes) gives the
package set; combined with the `doemerge` stage loop:

| target | overlay **category** | libc | kernel | toolchain pkgs |
|---|---|---|---|---|
| `…-linux-gnu` (GCC) | `cross-<CTARGET>` | `sys-libs/glibc` | `sys-kernel/linux-headers` | binutils, gcc, linux-headers, glibc |
| `…-linux-musl` **`-L`** | **`cross_llvm-<CTARGET>`** | `sys-libs/musl` | linux-headers | clang-crossdev-wrapper, compiler-rt, libunwind, libcxxabi, libcxx (+ musl) |
| `…-elf` (bare metal) | `cross-<CTARGET>` | `sys-libs/newlib` | **none** (`kernel_category=`) | binutils, gcc, newlib |

KEY behaviours to match:
- **LLVM uses a different category prefix `cross_llvm-<CTARGET>`** (not
  `cross-<CTARGET>`). em must resolve/route both.
- **`-L` rejects glibc** — `crossdev -L … -linux-gnu` errors "LLVM/Clang cannot
  currently compile glibc". LLVM ⇒ musl / newlib / llvm-libc only.
- **bare-metal (`-elf`)** has **no kernel-headers** stage; libc = newlib.
- This box has GCC targets installed (`cross-riscv64-unknown-linux-gnu`,
  `cross-riscv64-unknown-elf`); `cross_llvm-*` (LLVM) is not yet set up here.

### Host stage1 vs target sysroot — which config (clarified + validated)
- **Host stage1 (concern 3, the cross compiler/tools)**: just use the **HOST
  config**. `cross-<CTARGET>/{binutils,gcc}` are host-arch tools targeting
  `<CTARGET>`; the eclass does cross via the *category*. Validated:
  `em -p --root /tmp/e --config-root / cross-…/binutils` → from-scratch closure
  (`virtual/libintl`, `libiconv`, `zlib`, … all `N`). No special config needed.
- **Target sysroot (concern 1, libc/headers/runtimes)**: uses crossdev's
  **special make.conf** (`CHOST/CBUILD`, `ROOT=/usr/${CHOST}/`) + a target
  **profile link** (next item).
- The earlier `--root` NoSolution was self-inflicted: I fed the *special cross*
  make.conf (`--config-root /usr/riscv64…`, whose `ROOT=/usr/${CHOST}/` fought
  `--root`) to a *host stage1* build. Host stage1 wants host config; the special
  config belongs to the target-sysroot build.

## Profile linking for the target sysroot — ITEM TO ADDRESS

How crossdev-stages does it (the reference; `lib/sysroot.sh:84-93`,
`crossdev-stages/src/target.rs:123-160`, `cross-stage.sh:45-99`):
- **`eselect profile` CANNOT be used cross-arch** (host ARCH ≠ target ARCH) —
  explicit comment in `lib/sysroot.sh:90`. So the profile is linked by a **direct
  absolute symlink**:
  ```
  ln -s /var/db/repos/gentoo/profiles/<target-profile> <sysroot>/etc/portage/make.profile
  ```
- the target profile path is arch-specific, mapped from the tuple, e.g. riscv64 →
  `default/linux/riscv/23.0/rv64/lp64d` (`common.sh:258` `gentoo_profile`,
  `cross-stage.sh:45`).
- the Rust `target.rs` copies both the `make.profile` symlink and the `profile/`
  dir from the crossdev prefix (`/usr/<CHOST>/etc/portage`) into the target
  sysroot's portage config.

**em requirement:** concern-1 init/`--setup` for a cross target must (a) write the
special make.conf (`CHOST/CBUILD`), and (b) **link `make.profile` directly to the
target-arch profile in the repo — NOT via `eselect profile`** (which validates
against the host arch and fails). Need a tuple→profile mapping (reuse
crossdev-stages' `gentoo_profile`, or crossdev's `--show-target-cfg` arch). This
is also the missing piece behind concern 1's self-contained `--root` path.

### crossdev's hardcoded `embedded` profile is a SHORTCOMING — em follows the crossdev-stages fix
Read from the canonical `crossdev` sources (`emerge-wrapper` `cross_wrap_etc`):
crossdev links **`embedded` for *every* sysroot** —
`ln -snf ${MAIN_REPO_PATH}/profiles/embedded ${SYSROOT}/etc/portage/make.profile`
— regardless of target arch. Because `embedded` is arch-neutral, crossdev then
has to **inject what the profile would have provided** via a local `profile/`
subdir it ships in `/usr/share/crossdev/etc/portage/`:
- `profile/make.defaults`: `ARCH=<arch>`, `KERNEL="-linux <kernel>"`, `ELIBC=<libc>`
- `profile/use.force`: `-kernel_linux` + `kernel_<KERNEL>`
- (LLVM) appends `CC=<CHOST>-clang`, `LD=ld.lld`, `AR=llvm-ar`, … to make.defaults

That whole `profile/` dance exists **only to paper over the arch-neutral
`embedded` base** — it loses the arch profile's multilib/ABI/USE-default chain
(e.g. riscv `rv64/lp64d`), which crossdev then has to reconstruct per-package in
the multilib env files (`load_multilib_env`). **crossdev-stages fixes this**
(`lib/sysroot.sh:84-93`): it links the proper arch-specific `gentoo_profile`
(`default/linux/riscv/23.0/rv64/lp64d`) directly, so ARCH/ELIBC/KERNEL/ABI all
come from the profile and no `profile/` override is needed.

**em adopts the crossdev-stages fix** (`crossdev/target.rs::profile_path`): link
the arch-specific profile for OS targets; fall back to `embedded` **only** for
bare-metal (`-elf`/newlib, no kernel), where no `default/linux/<arch>` profile
applies. Consequently em does **not** need crossdev's `profile/make.defaults` +
`use.force` shim — the arch profile supplies ARCH/ELIBC/KERNEL. (LLVM's
`CC=<CHOST>-clang` toolchain vars from make.defaults are still relevant and
belong with the Stage-B build-env wiring, not the profile shim.)

### crossdev helper wrappers (read for Stage A/B build-env parity)
- **`cross-emerge`/`cross-ebuild`**: `CHOST` from argv0, `SYSROOT=/usr/$CHOST`,
  `PORTAGE_CONFIGROOT=$SYSROOT`, `CBUILD`+`BUILD_*FLAGS` from host `portageq`,
  `exec emerge --root-deps=rdeps`. (ROOT comes from the sysroot make.conf.)
- **`cross-pkg-config`** (`<CHOST>-pkg-config`): sets `PKG_CONFIG_SYSROOT_DIR=
  $SYSROOT`, `PKG_CONFIG_LIBDIR`/`PKG_CONFIG_SYSTEM_*` into `$ESYSROOT/usr/<libdir>`,
  libdir from `LIBDIR_${ABI}` (else probe `-print-file-name=pkgconfig`), and
  **rejects host `-I`/`-L`** in the output as a guard. em's build env must point
  pkg-config at the sysroot for cross target-package builds.
- **`cross-fix-root`**: post-install fixup — chmod sysroot libs, rewrite `.la`
  `libdir=`/`dependency_libs` and `*-config` `prefix=` to `$SYSROOT/usr`, and add
  `<CHOST>-`prefixed `*-config` symlinks. Relevant to the merge/postinst path.

## The two toolchain models (KEY: they are very different)

### GCC cross (`cross-<triple>/*`, crossdev's classic model)
Per-target *compiler binaries* (`<triple>-gcc`, `<triple>-as`/`ld`). crossdev
builds them as `cross-<triple>/{binutils,gcc,glibc|musl,linux-headers}` with a
**staged bootstrap** because gcc needs a libc that needs headers that need
binutils:
1. `cross-<triple>/binutils`
2. `cross-<triple>/linux-headers` (kernel UAPI)
3. `cross-<triple>/gcc` **stage1** (`USE=-* nostdlib`, C only, no libc)
4. `cross-<triple>/glibc` (or `musl`) — built with stage1 gcc
5. `cross-<triple>/gcc` **stage2** (full, links against the new libc)
The resolver orders these by deps, but the **stage1/stage2 gcc USE split** is
crossdev policy, not in the ebuild graph — em must drive it.

### LLVM/Clang cross (the simpler, preferred path — what "better llvm/clang" means)
`clang`/`lld` are **already cross-compilers**: one host binary targets any triple
via `--target=<triple> --sysroot=<sysroot>`. **No per-target compiler build.**
The cross toolchain is just the *target* runtime bits built into the sysroot with
the host clang cross-targeting:
1. `cross-<triple>/linux-headers` (or none for `-elf`/baremetal)
2. **libc for the target**: glibc / musl (or LLVM libc as a generic option),
   cross-built with host clang.
3. `compiler-rt` (builtins), `libunwind`, `libc++`/`libc++abi` for the target.
No stage1/stage2 dance: clang+lld already exist; we only produce the sysroot
contents. This makes LLVM cross dramatically less staged than GCC — lead with it.

## How crossdev actually works (read from `/usr/bin/crossdev`, 2057-line bash)

crossdev does **not** resolve or build anything itself — it (1) lays down the
overlay + config for `<CTARGET>`, then (2) drives a fixed sequence of
`emerge cross-<CTARGET>/<pkg>` calls with per-stage `USE`. The unit is:

```
doemerge <pn> [logsuffix]:
    set_use <pn> <USE>            # writes the per-pkg package.use into the overlay
    emerge cross-<CTARGET>/<pn> ${EOPTS}     # EOPTS = UOPTS + "-u"  (no --nodeps/--oneshot by default)
```
Global env it sets: `EMERGE_DEFAULT_OPTS=--quiet-build=n`,
`FEATURES="$FEATURES -stricter"`, `USE="$USE -selinux"`.

### Package set (chosen by tuple + `--llvm`)
GCC: `BPKG`=binutils, `GPKG`=gcc, `KPKG`=linux-headers, `LPKG`=libc
(glibc/musl/… from the tuple's LIBC field). LLVM: `CPKG`=clang-crossdev-wrapper,
`RPKG`=compiler-rt, `UPKG`=libunwind, `APKG`=libcxxabi, `PPKG`=libcxx. Each has a
matching `?USE` var (`BUSE`/`GUSE`/…) and stage-disable masks
(`GUSE_DISABLE_STAGE_1/2`).

### Stage sequence (the emerge calls)
GCC:
- **s0** binutils — `USE=$BUSE doemerge $BPKG`
- **s1** bare C compiler — (if `--with-headers`: kernel `headers-only`, then libc
  `headers-only` with `--nodeps`) then `USE="$GUSE $GUSE_DISABLE_STAGE_1" doemerge $GPKG-stage1`
- **s2** kernel headers — `USE="$KUSE headers-only" doemerge $KPKG`
- **s3** full libc — `USE="$LUSE $LUSE_DISABLE" doemerge $LPKG`
- **s4** full gcc — `EOPTS+=--newuse USE="$GUSE $GUSE_DISABLE_STAGE_2" doemerge $GPKG-stage2`

LLVM (`-L`): preflight asserts `llvm-core/llvm` installed AND the target arch is
in its `llvm_targets_*` USE; writes `/etc/clang/cross/<CTARGET>.cfg`
(`--sysroot=/usr/<CTARGET> --target=<CTARGET> @../gentoo-runtimes.cfg`;
`-static -fno-stack-protector` for llvm-libc). Then: s0 `$CPKG`
(clang-crossdev-wrapper), s1 `$RPKG` (compiler-rt), s4 `$UPKG`(libunwind
static-libs)/`$APKG`(libcxxabi)/`$PPKG`(libcxx). No stage1/stage2 gcc split —
clang is the cross compiler.

Extra (after stages): `--ex-gcc`→`$GPKG-extra`, `--ex-gdb`→`$DPKG`,
`--ex-pkg X`→`doemerge X`.

### KEY ARCHITECTURAL INSIGHT: em does NOT reimplement cross-compilation
`set_links` (l.1416) shows `cross-<CTARGET>/<pkg>` is a **symlink** to the real
`<cat>/<pkg>` ebuild dir (e.g. `cross-riscv64…/gcc` → `sys-devel/gcc`). The cross
magic (CHOST mangling, installing libc/headers into `/usr/<CTARGET>` while the
compiler lands on the host, stage gating) lives in the Gentoo **eclasses**
(`toolchain.eclass`, `toolchain-funcs`, cross handling), triggered by the
`cross-<CTARGET>` **CATEGORY**. em already resolves these symlinked ebuilds
(multi-repo + `follow_links(true)`, per [[project-dep-resolver]]). So em's builder
does **not** cross-compile by hand — it runs the ebuild phases for
`cross-<CTARGET>/<pkg>` (brush already sources the eclasses) and the eclass does
cross. em's real additions are: (A) the cross config/env entry, (B) running those
phases ROOT-correctly, (C) the per-stage `USE` (`headers-only`, stage1/stage2
disables) that crossdev injects via `set_use`.

### KEY DESIGN DECISION for em
crossdev's `doemerge` calls literal `emerge`. So there are two ways em fits:
1. **em as the emerge backend** crossdev drives (lightest): em just needs to
   correctly cross-build ONE `cross-<CTARGET>/<pkg>` (Stage A+B). Then real
   `crossdev -t <tuple>` + `<CTARGET>-emerge <pkg>` (and `--ex-pkg`) work by
   pointing at em. Immediately useful; em owns the *build*, crossdev owns the
   *orchestration*.
2. **em replicates the orchestration** (`em --cross -t <tuple>`): em owns the
   overlay/config setup + the stage loop too. More work; only needed if we want
   to drop the crossdev bash dependency.
Recommend **(1) first** — Stage A+B unlocks `<CTARGET>-emerge`/`--ex-pkg` and is
the foundation for (2). Stage C (the stage loop) is the (2) increment.

### Still to read in the script (next)
`parse_target` (l.142, tuple→vars), `setup_portage_vars` (l.658),
`set_links`/`set_use_force`/`set_use_mask`/`set_metadata` (l.1416–1547, the
overlay/symlink/config writers), `load_multilib_env` (l.1212).

## First prerequisites — three Stage-0 setup tools (= `--init-target`, no build)

STATUS: **DONE (2026-06-20).** Shipped as the `em crossdev` subcommand
(`src/crossdev/{mod,target}.rs`): `--show-target-cfg` (preview, no writes) and
`--init-target` (lay down everything). `CrossTarget::parse` does the tuple →
category / package-set / `gentoo_arch` / profile / `CFLAGS` derivation (glibc
`gnu`, `musl`, bare-metal `-elf`/`-eabi` newlib; `-L` ⇒ `cross_llvm-*` and
rejects glibc). Install root follows the em root model: sysroot =
`<EROOT>/usr/<CTARGET>` (`/usr/<CTARGET>` default; `--local`/`--prefix`/`--root`
retarget). All three tools below implemented + idempotent; verified against a
sandboxed `--config-root`/`--root` (overlay symlinks/metadata/categories/
repos.conf match the live crossdev overlay byte-for-byte; sysroot make.conf has
`CBUILD`/`CHOST`/`CTARGET`/`ARCH`/keywords/`CFLAGS`; make.profile is a direct
absolute symlink). 4 unit tests in `target.rs`. **Next: Stage B** (cross-build
one target package end-to-end).

The foundation to build FIRST (pure filesystem setup, independent of the
resolver). They compose into a no-build `em crossdev --init-target` /
concern-1+2 init:

1. **Repo-management tool — create the crossdev overlay.** Lay down
   `cross-<CTARGET>` (or `cross_llvm-<CTARGET>` for `-L`): per-package symlinks to
   the real ebuild dirs (crossdev `set_links`), `metadata/layout.conf` +
   `profiles/{repo_name,categories}` (`set_metadata`), and a `repos.conf` entry.
   *em today:* `ReposConf` (repos_conf.rs) **reads** only — overlay **creation is
   NEW** (symlinks + metadata + repos.conf write).
2. **Confdir-creation tool — write the cross make.conf.** Write the special
   `<sysroot>/etc/portage/make.conf` (`CHOST`, `CBUILD`, `ROOT=/usr/${CHOST}/`,
   `CFLAGS`, …) — crossdev `set_metadata` / crossdev-stages `target.rs`.
   *em today:* **mostly EXISTS** — `MakeConf::{set,save}` (make_conf.rs) +
   `setup.rs::bootstrap` already write a prefix make.conf. Reuse for cross values.
3. **Profile-management tool — link the profile in the confdir.** Symlink
   `<sysroot>/etc/portage/make.profile` → `…/gentoo/profiles/<target-profile>`
   **directly** (NOT `eselect profile` — fails cross-arch), plus the
   tuple→profile mapping.
   *em today:* reads `make.profile` everywhere; has symlink helpers
   (`setup.rs::link_host_*`, `std::os::unix::fs::symlink`) + an `eselect` wrapper
   (`select`) — but the **cross-arch direct `make.profile` symlink + tuple→profile
   map are NEW**.

Net: tool 2 ≈ done (reuse `MakeConf`+`setup.rs`); **tools 1 and 3 are the new
build**, both pure FS setup ⇒ a clean, testable first slice with no resolver
dependency. Sequencing: 1 → 2 → 3, then they wire into `em crossdev --init-target`.

## Stage A/B findings — the real `<CTARGET>-emerge` wrapper (2026-06-21)

Read from the installed `/usr/bin/<CTARGET>-emerge` (the authoritative driver):

```sh
CHOST=<tuple>                       # from argv0
SYSROOT=${BROOT}/usr/${CHOST}       # = /usr/<CHOST>
PORTAGE_CONFIGROOT=${SYSROOT}       # config (profile/make.conf) FROM THE SYSROOT
# CBUILD + BUILD_CFLAGS/CXXFLAGS/CPPFLAGS/LDFLAGS: queried from the HOST
#   (portageq envvar with CHOST/SYSROOT/CONFIGROOT unset)
exec emerge --root-deps=rdeps "$@"
```

- **`ROOT` is NOT set by the wrapper** — it comes from the sysroot `make.conf`
  (`ROOT=/usr/${CHOST}/`). So the sysroot make.conf's `ROOT` matters (our
  EPREFIX-aware `ROOT` write feeds this).
- **`--root-deps=rdeps` is the crux**: only **RDEPEND** is installed into the
  target ROOT (the sysroot); **DEPEND/BDEPEND resolve against the build host
  (`/`)**. This is the inverse of the toolchain case (`host_copies.rs`, which
  pushes host build-copies): here the *bulk* is host build-deps and only runtime
  deps land in the sysroot.
- `BUILD_*FLAGS`/`CBUILD` come from the host config; `C*FLAGS` from the sysroot.

### em gaps blocking target-package cross builds
Probed live (`em -p --config-root /usr/<CHOST> --root /usr/<CHOST> sys-libs/zlib`
→ `NoSolution`/`NoVersions` over `(Unbounded,Unbounded)` on `merge_root: Target`):

1. **FIXED (2026-06-21) — keyword acceptance used the host arch, not the
   target.** The real blocker: `AcceptKeywords::new` was keyed to the global
   `--arch` (host, e.g. arm64), so a target package keyworded `~riscv`/`riscv`
   was filtered out for a riscv sysroot ⇒ every target package = NoVersions.
   Decoded by instrumenting: cpn was `sys-libs/zlib`, `in_data=true`, yet served
   no Target versions; `--arch riscv` made it resolve. Fix (`depgraph/mod.rs`):
   when `cross.active`, derive the acceptance arch from the sysroot `CHOST`
   (`Arch::from_chost`) instead of `--arch`. Now `em -p --config-root <sysroot>
   --root <sysroot> <pkg>` resolves the full target closure into the sysroot with
   no manual `--arch`. (The sysroot `repos.conf` from `--init-target` is still
   wanted so `PORTAGE_CONFIGROOT=<sysroot>` sees the tree, but repo discovery
   currently falls back to the host `ReposConf::load()`, so it was not the gating
   bug.)
2. **FIXED (2026-06-21) — `--root-deps=rdeps` semantics.** A genuine cross-*arch*
   target build now discards the target package's DEPEND (build-only) from the
   sysroot graph, matching crossdev's `<CTARGET>-emerge --root-deps=rdeps`: only
   RDEPEND/PDEPEND install into the sysroot; build deps resolve on the host
   toolchain (BDEPEND was already host-routed). Plumbing: new provider flag
   `root_deps_rdeps` (`set_root_deps_rdeps`), consumed in
   `cross_target_runtime_deps` (drops `by_class[0]`). Gated in `depgraph/mod.rs`
   to `cross_arch != host arch` so same-arch offset/stage builds (`--root
   stage1/`, also `cross.active`) keep DEPEND → target ROOT. **Subtlety:** the
   synthetic solver root's seed targets live in `by_class[0]` and the root reports
   `MergeRoot::Target`, so rdeps must be suppressed for `package.is_virtual()` —
   otherwise it drops the user's requested targets and the whole solve collapses
   to empty (hit this; fixed with the `&& !package.is_virtual()` guard at both
   call sites). Unit test `root_deps_rdeps_drops_target_depend` covers on/off.

   **Also found — empty cross plan = missing target VDB.** If `<sysroot>/var/db/pkg`
   does not exist, the installed loader falls back to the **host** VDB, so
   host-installed packages (zlib, libpcre2…) wrongly satisfy target requests and
   the plan comes up empty. `--init-target` now `mkdir -p`s the empty target VDB
   (`write_sysroot_config`); a fresh sysroot then resolves `[ebuild N ... to
   <sysroot>/]` correctly. (Manual sysroots need the dir too.)

The cross compiler (`<CHOST>-gcc`/`-ld`) is already on `PATH`, so once
resolution is fixed the eclass-driven build should follow. NB: cross
**toolchain** packages (`cross-*/binutils|gcc`) are HOST builds — resolve them
with **host** config (config-root=`/`), NOT the sysroot (that NoSolution is
self-inflicted, see "Host stage1 vs target sysroot" above). Only **target**
packages use the sysroot config + `--root-deps=rdeps`.

**Recommended Stage-B order:** (1) sysroot `repos.conf` in `--init-target`
[DONE, commit 9407925], (2) `--root-deps=rdeps` root routing [DONE — see gap #2;
keyword-arch fix + rdeps drop + target-VDB dir], then **(3) NEXT: the
`<CTARGET>-emerge` wrapper / `em --cross <tuple>` entry point** [Stage A
ergonomics] + verify one leaf target lib actually builds and `file` reports the
target arch [Stage B build shell].

## Implementation stages

### Stage A — cross entry point (`{target}-emerge` equivalent) — DONE (2026-06-21)
Implemented as a global `--cross <tuple>` flag (chose the flag over argv0
`<tuple>-emerge`: friendlier, and em has no per-target symlinks). It is **sugar**
over the existing root model: `Cli::roots()` layers the cross sysroot
`<EROOT>/usr/<tuple>` on top of `base_roots()` as `config == base == target`
(crossdev's `PORTAGE_CONFIGROOT == ROOT == SYSROOT`). `<EROOT>` still comes from
`--local`/`--prefix`/`--root`, so `em --local --cross <t>` targets
`~/.gentoo/usr/<t>`. CHOST/CBUILD + `--root-deps=rdeps` then fall out of the
existing `root_aware::detect` (reads the sysroot make.conf) — no extra plumbing.
`run_emerge` pre-flights the sysroot (`<sysroot>/etc/portage/make.conf` exists)
and otherwise bails with `run: em crossdev -t <tuple> --init-target`. Tests:
`cli::tests::cross_*`. Verified live: `em --root <eroot> --cross <t> -p zlib`
→ `[ebuild N] ... to <eroot>/usr/<t>/`, header shows
`CHOST=riscv64-… CBUILD=aarch64-…`.

**Correction (the build shell already sources our confdir):** the build-time env
is NOT a separate Stage-B task — it falls out of the existing merge path once
`--cross` points config/root at the sysroot:
- `ebuild.rs` `apply_profile_env(config_root=…/usr/<tuple>)` sources the sysroot
  make.defaults chain + make.conf, so phases see **CHOST, CBUILD, target
  CFLAGS/LDFLAGS, ABI, USE_EXPAND** — straight from the make.conf `--init-target`
  wrote. The cross `package.env`/`env/*.conf` is sourced too.
- `set_build_roots(config_root, build_sysroot=None, eprefix=None)`: with
  `config == base == target` (build_sysroot None), `shell.rs` sets
  `SYSROOT = ESYSROOT = ROOT = <sysroot>` and `BROOT = "/"` — exactly crossdev's
  `SYSROOT=ESYSROOT=ROOT=/usr/<CHOST>`, `BROOT=/`. `econf` then passes
  `--host=$CHOST --build=$CBUILD`.
- `${CHOST}-gcc` (`tc-getCC`) is already on the host PATH from the installed
  `cross-<tuple>/gcc`.

So Stage B is not env wiring — it is just *running a real build and verifying*:
the toolchain is installed, the compiler is actually invoked cross, and the
artifact is the target arch (`file`). Watch for the genuine gaps: BDEPEND build
tools resolving on BROOT (already handled by the dual-root solver + `--root-deps
=rdeps`), and the cross toolchain PATH when it is NOT in `/usr/bin` (LLVM
`--target`/`--sysroot` model).

### Stage B — cross builder (one leaf target package) — DONE (2026-06-21)
Verified end-to-end: `em --cross riscv64-unknown-linux-gnu sys-libs/zlib`
produces a real **RISC-V** `libz.so` (`file` → `ELF … UCB RISC-V, double-float
ABI`) merged into the sysroot, on an aarch64 host with the crossdev toolchain
installed.

The one real gap the build flushed out (everything else was already wired):
- **Toolchain selection.** The env wiring (CHOST/CBUILD/CFLAGS from make.conf,
  SYSROOT=ESYSROOT=ROOT, BROOT=/) was all correct, but the build still used the
  **host `gcc`** → a host-arch artifact. Root cause: `tc-getCC` only exports
  `CC=${CHOST}-gcc` when something calls it, and for a *single-ABI* target the
  multilib `DEFAULT_ABI` path skips `multilib_toolchain_setup`'s CC export, so an
  ebuild that builds with a raw `./configure` (zlib) never gets the cross CC.
  Diagnosed with a probe ebuild: CHOST **did** propagate (`CHOST=riscv64-…`), so
  it was purely CC selection. Fix (`portage-repo` `shell.rs::init_build_env`): when
  cross (CHOST≠CBUILD, both set) and `${CHOST}-gcc` is on PATH, proactively export
  `CC/CXX/AR/NM/RANLIB/STRIP/OBJCOPY/OBJDUMP/READELF/LD = ${CHOST}-<tool>` unless
  already set — em's standing-in for `tc-getCC`. Native builds (CBUILD unset or
  ==CHOST) untouched.
- Header note: the cross gcc's baked-in `--sysroot` (`/usr/<tuple>`) already has
  the headers/libc, so a clean `--cross` build (SYSROOT==ROOT==populated sysroot)
  resolves `sys/types.h` etc. A split `--config-root X --root Y(empty)` leaves
  SYSROOT empty and fails — not a bug, just don't point ROOT at an unpopulated
  tree for a from-scratch build.

Still TODO for Stage B breadth:
- **LLVM**: host `clang --target=<triple> --sysroot=$ESYSROOT`, `lld` — needs
  wiring (no `<triple>-clang` unless `clang-crossdev-wrappers` is installed); the
  GCC `${CHOST}-<tool>` export above does not cover the clang model.
- A leaf with real deps (confirm RDEPEND libs resolve from the sysroot and
  BDEPEND tools from BROOT during an actual build, not just `-p`).

## `em crossdev --setup` — full cross environment (design, 2026-06-21)

The full cross `--setup` is **two phases** (the user's two items), which crossdev
+ crossdev-stages keep terminologically distinct (`docs/design.md`):

1. **Toolchain creation** → the **crossdev prefix** `/usr/<chost>` on the **host**
   (`ROOT=/`, config from host). The cross compiler + headers + stage1 libs. This
   is "Stage C" below (the staged binutils→headers→gcc1→glibc→gcc2 bootstrap).
   *Not* a sysroot — "sysroot" is reserved for the `--sysroot` compiler flag.
2. **Sysroot/target-stage creation** → the **target stage** rootfs at `ROOT=/target`,
   built with `em --cross <tuple>` (= `<chost>-emerge`). Two sub-modes mirroring
   catalyst: **stage1** (bootstrap: `baselayout` `USE=build` → `packages.build` →
   `portage`) and **stage3** (`@system`/`@world`). Optionally **seeded** from a
   target-arch stage3 tarball (the unused `gentoo-stages` downloader).

**Why "both share the stage1/stage3 problems":** each is an *ordered build into a
root the solver can't fully resolve against* (nothing installed yet), so neither
is pure solver output. The shared problems, from `cross-stage.sh`:
- **Ordered, curated build lists** with explicit bootstrap order (not a plain atom
  set) — the chicken-and-egg (headers-only, `--nodeps`, two-stage gcc/glibc).
- **`USE=build`** minimal pass for `baselayout`/`portage` (and stage1 gcc USE).
- **Binpkg** (`-b -k`) to cache+reuse across the staged passes.
- **Per-root profile selection** (`eselect profile set` on each root).
- **`merge-usr --root`** (crossdev prefix starts split-usr).
- **The `--sysroot=$EROOT` LDFLAGS workaround** for hosttools like `perl`.

**The shared abstraction em needs — an ordered `StagePlan` driver.** A `StagePlan`
is `Vec<StageStep>` where `StageStep { atoms, use_override, env_tag, nodeps,
binpkg, root }`; the driver runs each step through the *existing* build/merge path
(the one Stage B verified) against the step's root. Both phases are `StagePlan`
templates:
- **toolchain plan** → host config, `ROOT=/`, installs into `/usr/<chost>` via the
  `cross-*` overlay category; the GCC/LLVM stage list with per-stage `USE` + the
  `*-stage1`/`*-headers` env tags (`write_cross_env` already lays some down).
- **sysroot stage1 plan** → `--cross` (config/root = target), `USE=build`
  baselayout→packages.build→portage; **stage3 plan** → `@system`/`@world`.

**Reuse map (already in em):** the build shell + cross toolchain selection (Stage
B), `--cross` entry point, `--root-deps=rdeps`, `--init-target` FS setup, the
completed `Solver` trait, and `gentoo-stages` for the seed tarball. **New:** the
`StagePlan` type + driver, the two templates, and `-b/-k` binpkg + `USE=build`
plumbing in the driver.

**Proposed CLI surface** (em owns the build engine + standalone path; crossdev-
stages stays the rootless-sandbox/image orchestrator that calls em):
- `em crossdev <tuple> --setup` (alias `--stage4`): toolchain bootstrap into the
  prefix. `--stage0..--stage3` stop earlier (binutils-only … libc).
- `em crossdev <tuple> --sysroot[=stage1|stage3] [--seed <stage3.tar>] --root DIR`:
  build the target stage. (Or fold under `em --cross <tuple> --stage1`.)

**Implementation sequence:** (1) `StagePlan`/`StageStep` + driver over the existing
merge path; (2) `--nodeps` + per-step `USE` override + `-b/-k` plumbing; (3) the
toolchain template (Stage C list) → `--setup`; (4) the stage1/stage3 templates →
`--sysroot`; (5) seed-from-stage3 via `gentoo-stages`. Stage 1+2 are the shared
core both phases depend on — build it first.

### Stage C — toolchain bootstrap (the real crossdev workflow) — LARGE
NB: Stage B built a **leaf lib against an already-bootstrapped toolchain**. The
toolchain itself needs crossdev's staged bootstrap, which em does NOT yet do —
it is the chicken-and-egg part (a compiler needs a libc, a libc needs a
compiler). em's plain dependency solver can't express it; it needs an explicit
**ordered bootstrap driver** with per-stage USE/env, mirroring crossdev.

**Canonical crossdev GCC sequence** (`/usr/bin/crossdev` `doemerge` loop,
lines ~1939-2049), staged `is_s0..is_s4`:
1. **binutils** (`USE=${BUSE}`) — the assembler/linker, no compiler yet.
2. **stage1 (bare C compiler)**: first **kernel-headers** (`headers-only`) then
   **libc headers** — glibc with `USE="headers-only"` **and `--nodeps`** to break
   the glibc→newer-gcc dep cycle (`${LPKG}-headers`). Then **gcc-stage1**
   (`${GPKG}-stage1`) with `GUSE_DISABLE_STAGE_1 = -fortran -d -go -jit -cxx
   -openmp -sanitize -zstd -zlib …`: a freestanding C compiler, no libc.
3. **stage2 (kernel headers)**: full `linux-headers` (`${KPKG}-stage1/stage2`).
4. **stage3 (libc)**: full **glibc/musl**, compiled by gcc-stage1.
5. **stage4 (full compiler)**: **gcc-stage2** (`${GPKG}-stage2`,
   `GUSE_DISABLE_STAGE_2 = … -sanitize`) linked against the just-built libc; the
   final `<triple>-gcc`. (LLVM model: `compiler-rt` at stage1, then
   `libunwind`/`libcxxabi`/`libcxx` at stage4 — no two-stage gcc.)

Two bootstrap tricks em must reproduce: **two-stage glibc** (headers-only →
full) and **two-stage gcc** (stage1 no-libc → stage2 full). crossdev expresses
the per-stage build via `USE="…" doemerge ${PKG} ${PKG}-stageN`, where the
`-stageN` tag selects a `/etc/portage/env/<cat>/<pkg>` file crossdev wrote
(`*-stage1`/`*-headers`/`*-quick`). em equivalent: drive the same ordered list
with per-step USE overrides + the cross env files (`write_cross_env` already lays
some down), gated `--root-deps=rdeps` + `--nodeps` where crossdev uses them.

**LLVM first** (simpler, no two-stage gcc): headers → target libc (glibc/musl) +
`compiler-rt`/`libunwind`/`libc++` into the sysroot, all with host clang.

Decide how em expresses "build me a `<triple>` toolchain" — a `--toolchain`/
`--stageN` mode on `em crossdev` (closest to crossdev) vs a set/meta target
(`@cross-toolchain`). Leaning `em crossdev <t> --stage4` driving the ordered
list, since the per-stage USE/env doesn't fit a plain atom set.

### Stage D — true dual-root scheduling — LARGE, deferred
Independent `PackageData` per root so a CPV needing both host-native and
target-cross builds is two plan entries (`root-model.md` § Cross). The post-solve
host walk (`host_copies.rs`) covers the common case; only revisit if a real cross
build needs the same CPV on both roots.

## LLVM/Clang cross specifics (the "better llvm/clang" ask)

- Treat the **LLVM path as first-class**, arguably the default for new targets:
  no per-target compiler build, just sysroot population with host clang.
- Clang multi-version (20/21/22) is installed; pick the active one via the
  existing `llvm_slot`/`LLVM_SLOT` machinery (already handled in USE_EXPAND /
  Level-C work).
- `clang-toolchain-symlinks` / `clang-linker-config` already provide the
  `<triple>-clang` symlink + linker wiring — reuse rather than reinvent.
- baremetal/`-elf` targets (e.g. `riscv64-unknown-elf`): no kernel headers / no
  full libc — LLVM (compiler-rt + picolibc) is the natural fit.

## "ex-pkg" — resolved: it's crossdev's `--ex-pkg`

`--ex-pkg <pkg>` (crossdev `--help`, "Extra Fun" section, with `--ex-gcc`/
`--ex-gdb`) builds **extra packages onto an already-established cross target**,
after the toolchain stages. In the script it is just:
```
for pkg in "${XPKGS[@]}" ; do doemerge "${pkg#*/}" ; done   # = emerge cross-<CTARGET>/<pkg>
```
i.e. nothing special — the same per-package cross build as everything else, run
after stage4. So "support ex-pkg" == "em can build an arbitrary package into an
existing `<CTARGET>` sysroot", which is exactly Stage A+B below (the
`<CTARGET>-emerge <pkg>` path). `--ex-gcc`/`--ex-gdb` are the same with the
gcc/gdb atoms (`GPKG`-extra / `DPKG`).

## Sequencing / first steps

1. Validate current cross `-p` on this host (`riscv64` gcc = 18 pkgs) — baseline.
2. **Stage A** — cross entry point (cheap unlock).
3. **Stage B** — cross-build ONE leaf target package (LLVM path first), verify
   the artifact arch.
4. **Stage C (LLVM)** — populate a `<triple>` sysroot (target libc + runtimes).
5. **Stage C (GCC)** — the staged binutils/gcc/glibc bootstrap.
6. Stage D only if a concrete build needs it.

## Coordination

Mostly cli/build path (`ebuild.rs`, `root_aware.rs`, `overlay.rs`, cli entry) +
config. Touches the resolver only lightly (per-class root routing is already in
place via Tier-1). Keep Stage A/B independent of the resolver-abstraction work.
