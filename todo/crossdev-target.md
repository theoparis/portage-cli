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

## Decomposition: three orthogonal concerns (the planning frame)

crossdev conflates three things into one stage loop. em should treat them as
**separate concerns split by install root**, each mapping to an existing em
primitive:

### 1. Populate the `/usr/<CTARGET>` sysroot — *Target root, ≈ `--local --setup`*
The target-arch artifacts: kernel-headers, libc (glibc/musl), and the runtimes
(compiler-rt, libunwind, libc++/libc++abi). They install **into the sysroot**
(`ROOT=/usr/<CTARGET>`), cross-built. This is em's ROOT-offset build of target
packages — the `--root`/`--local` machinery — plus a `--setup`-style bootstrap to
lay down the empty sysroot's base config (crossdev writes
`/usr/<CTARGET>/etc/portage/{make.conf,package.*}`; em's `--local --setup`
already bootstraps a prefix — reuse that path, see `setup.rs`).

### 2. The `<CTARGET>-emerge` driver — *the wrapper / entry point*
Recognise the cross invocation and set
`CHOST/CBUILD, SYSROOT=ESYSROOT=/usr/<CTARGET>, BROOT=/, ROOT=/usr/<CTARGET>`
(overridable), then run em's normal resolve+build with per-class root routing
(already in place via Tier-1). Thin — config plumbing.

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

## Implementation stages

### Stage A — cross entry point (`{target}-emerge` equivalent) — SMALL
Recognise a cross invocation and wire the location vars from the crossdev config:
- trigger: argv0 `<tuple>-emerge`, or an explicit `em --cross <tuple>` (decide;
  argv0 matches portage, `--cross` is friendlier).
- set `CHOST=<tuple>`, `CBUILD=<host tuple>`, `SYSROOT=ESYSROOT=/usr/<CHOST>`,
  `BROOT=/`, `ROOT=/usr/<CHOST>/` (overridable), `PORTAGE_CONFIGROOT=/usr/<CHOST>`.
- today this is hand-driven as `em -p --root /usr/<CHOST> --config-root
  /usr/<CHOST>`; Stage A makes it a real entry point. Mostly config plumbing in
  the cli + `root_aware.rs`/`overlay.rs`.

### Stage B — cross builder (one target package end-to-end) — MEDIUM
The novel piece: the build shell (`ebuild.rs run_phase`) for a target task sets
`CHOST/CBUILD/SYSROOT/ESYSROOT/BROOT` and puts the right compiler on PATH:
- **GCC**: `<triple>-gcc` from the installed `cross-<triple>` toolchain.
- **LLVM**: host `clang --target=<triple> --sysroot=$ESYSROOT`, `lld`.
Prove it by cross-building a single leaf target lib into `/usr/<CHOST>` (e.g.
`cross-riscv64-unknown-linux-gnu/zlib`-style) and checking the artifact is the
target arch (`file`). Validates env + toolchain wiring before the toolchain dance.

### Stage C — toolchain bootstrap (the real crossdev workflow) — LARGE
Drive the staged builds above. Split by model:
- **LLVM first** (simpler): headers → target libc (glibc/musl) +
  `compiler-rt`/`libunwind`/`libc++` into the sysroot, all with host clang. A
  working `<triple>` sysroot you can `clang --target` against.
- **GCC**: the binutils→headers→gcc1→libc→gcc2 sequence with the stage1/stage2
  USE toggling driven by em (crossdev replicates portage policy here).
Decide how em expresses "build me a `<triple>` toolchain" — a set/meta target
(`@cross-toolchain`?) vs explicit package list vs a `--toolchain` mode.

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
