# `em --root` characterization

Tracking the "build a clean tree into an empty ROOT" path — the staging ground
for both the stage1 exercise and completing `--local`. Updated as findings land.

## Setup

Native ROOT-offset build: `em -p --root <empty> --config-root / <atoms>`.
The host (`/`) provides the compiler/toolchain (the seed — same model as
crossdev; no stage tarball needed). `BROOT=/` is always the host.

## What works (2026-06-15)

- **Mechanism validated.** `em -p --root <empty> sys-libs/zlib` → 1-package
  plan. Single packages whose deps are all in the host base resolve and
  (per the firefox work) build correctly.
- **`@set` expansion landed** (`90803fb`). `em -p --root <empty> @system`
  resolves to 316 packages — gcc/binutils/glibc/toolchain all present.
  Set expansion itself is correct (verified: `@system` matches the profile's
  `*`-marked `packages` lines; BDEPEND over-pull is a separate item below).
- **ROOT annotation in the plan** (`af50e10`). Every line shows
  ` to <ROOT>/` when `ROOT != /`, matching emerge.

## Tier-A userspace subset (2026-06-15)

`em -p --root <empty>` on a core slice
(`sed grep findutils xz-utils bzip2 gzip tar coreutils gawk file procps
diffutils less`) → ~50-package plan, all `N`. Resolved correctly: pulled in
the expected transitive closure (nettle, gnutls, gnupg, make, ...).

## @system divergence vs emerge — BDEPEND over-pull (RESOLVED 2026-06-18)

Was: `em -p --root <empty> --config-root / @system` = **316** packages vs
`emerge … --with-bdeps=n` = **182**, the ~125-pkg diff being almost entirely
**BDEPEND** (build-only deps): autoconf, automake, libtool, cmake, meson,
ninja, dev-lang/perl, rust-bin, docbook-*, …

**Fixed by** the `--with-bdeps` flag (commits `438f9c5`, `d38b0d3` Stage 3b
dual-root). em now defaults to `with_bdeps=false`, which strips host-provided
BDEPEND edges (`BROOT=/`) — matching `emerge --with-bdeps=n`.

**Verified 2026-06-18:** `em -p --root <empty> --config-root / @system` = **182**,
exact parity with `emerge … --with-bdeps=n @system` = 182. Pass `--with-bdeps`
to include the build-tool closure (per-edge `host_installed` filtered).

Root cause was as suspected (BDEPEND under ROOT-offset, not a set-expansion bug).
See [`docs/root-model.md` § BDEPEND / crossdev] and `depgraph/bdepend_trim.rs`.

## Roadmap sequencing (see `docs/root-model.md` § Sequencing)

The multi-root targets are staged by what each needs; each tier reuses the
previous. This doc is **Tier 1** (the active focus).

1. ~~**BDEPEND under ROOT-offset**~~ — *done*: em treats host-provided BDEPEND
   as satisfied by default (`with_b_deps=false`), matching emerge. `@system`
   parity reached (182 == 182). The remaining offset question is item 2, not
   BDEPEND routing in general.
2. **Tier 1 finish — unsatisfied-BROOT Host scheduling (OPEN).** An offset
   unsatisfied `BDEPEND`/`DEPEND` edge on `BROOT` must schedule a native merge
   to `/` (`MergeRoot::Host`), not be broot-filtered. This is the offset
   `@system` gap (177 vs 180; `nghttp2/nghttp3/ngtcp2`) — see
   `nonemptytree-bdeps-gap.md`. Reaching 180 == 180 closes Tier 1.
3. **Tier 2 — crossdev on top of Tier 1** (`{target}-emerge`, `CBUILD ≠ CHOST`).
   Same `(cpn, slot, root)` routing as item 2 with a foreign `CHOST`; cross
   already matches `riscv64-emerge -p gcc` (18 pkgs). Reuses Tier 1's
   Host-merge scheduling; not started beyond the `-p` match.
4. **Tier 3 — `--local` / `--prefix` (non-Gentoo host).** `BROOT` becomes a
   stage1 build-tool subset installed *into* the prefix (sharing host libc);
   `--setup` bootstraps it (today it borrows host tools via symlinks). Reuses
   Tier 1/2 routing. Most work, least general, deliberately last.

## Proven earlier (context, not this session)

- The firefox closure builds into a `--prefix`/`--local` target (the
  accumulated bashrc recipe in `setup.rs`); this is the baseline that `--root`
  characterization builds on.

## Stage1 from-scratch into `--root` (host arch) — STARTED (2026-06-25)

Goal: build a from-scratch stage1 into an empty `--root` (host arm64 first, so it
chroot-validates), per Catalyst + crossdev-stages. See [[catalyst-stage1-recipe]].

**Recipe (Catalyst `targets/stage1/chroot.sh`, crossdev-stages `target.rs`):**
`ROOT=<root>`, seed `/` = toolchain (== `em --root <root> --config-root /`);
`baselayout` (`USE=build`) first; `emerge --implicit-system-deps=n --oneshot
<profile packages.build>` with `USE="-* build"` `FEATURES="nodoc noman noinfo"`;
then `portage`; then `ldconfig -r <root>`.

**ldconfig — DONE (`dc68783`):** `env-update` now writes `${ROOT}/etc/ld.so.cache`
via the `ldconfig` *library* (lu-zero's crate) instead of shelling to the host
`ldconfig -r`. Arch-correct (reads each ELF) → works for a foreign-arch `--root`
too; no host binary dependency.

**Gotcha found: host-satisfied `@system` build deps vs the pre-flight check.**
`em --root <empty> --config-root / sys-libs/glibc …` fails pre-flight with
`glibc needs virtual/os-headers` — em host-satisfies `@system` DEPENDs during
resolution (doesn't pull them into the empty ROOT), but the pre-flight DEPEND
check requires them present in the ROOT. Adding `sys-kernel/linux-headers` to the
plan is **not** enough (em doesn't treat the provider as satisfying the virtual,
and won't auto-pull the virtual); listing `virtual/os-headers` explicitly works.
Catalyst sidesteps this by listing the whole bootstrap order in `packages.build`.
The proper fix is emptytree-style ROOT semantics: for a from-scratch ROOT, build
the DEPEND closure into the ROOT rather than host-satisfying it (ties into Tier-1
item 2 — unsatisfied-BROOT/DEPEND scheduling).

First validation target (minimal, chroot-able): `virtual/os-headers glibc bash
coreutils baselayout` → 16-pkg plan, builds; chroot test pending.

## Stage1 host arm64 — WORKING chroot (2026-06-25)

Minimal stage1 (`virtual/os-headers glibc bash coreutils baselayout` + deps) into
`/var/tmp/stage1-arm64` → `sudo chroot` runs **bash 5.3.15**, `ls /bin`,
`uname -mo` (aarch64). Three em bugs fixed to get a *self-contained* root:

- **RO-distdir not linked into DISTDIR** (`8a9558b`) — bash's `bash53-NNN`
  patches were in the host RO `/var/cache/distfiles`; fetch said "already
  present" but never exposed them in DISTDIR → `eapply` failed. Now symlinked in.
- **info `dir` collision** (`f34e046`) — stripped from images.
- **`usev !flag` ignored the `!` negation** (`ae42693`) — the "host-satisfied-dep"
  libcap-for-ls case turned out to be THIS: coreutils' `$(usev !caps
  --disable-libcap)` emitted nothing, configure autodetected the host libcap, and
  `ls` needed a `libcap.so.2` absent from the ROOT. Fixed → `ls` needs only
  libc+loader.

The `glibc needs virtual/os-headers` pre-flight is **by design** (glibc builds
against headers in the ROOT sysroot; SYSROOT=ROOT for a `--root` build), so the
bootstrap virtuals must be listed explicitly — exactly what `packages.build`
does; not a bug. (A stage1 still links seed libs for anything not yet in the
ROOT — catalyst's stage2 rebuild is what makes it fully self-hosting; out of
scope here.)

## Full `packages.build` stage1 — blocked on DEPEND-into-ROOT (design fork, 2026-06-25)

`em --root /var/tmp/stage1-full --config-root / --oneshot <packages.build>` (147-pkg
closure incl. the toolchain) fails the **pre-flight** with:

```
util-linux  needs: acct-group/root
libarchive  needs: sys-fs/e2fsprogs[abi_*(-)?]
libxcrypt   needs: sys-libs/glibc[-crypt(-)]
gcc         needs: sys-libs/glibc[cet(-)?]
```

Root cause: em host-satisfies `DEPEND` against the host VDB during resolution, so
(a) some real DEPENDs (`acct-group/root`, `e2fsprogs`) are never pulled into the
plan, and (b) the DEPEND edges that *are* pulled don't constrain ordering —
**glibc lands at plan pos 103, after its dependents libxcrypt (99) and gcc
(100)**. The pre-flight (correctly, for `SYSROOT=ROOT`) requires each DEPEND in an
*earlier* plan entry, so it flags these.

This is the long-noted "build the DEPEND closure into the ROOT vs host-satisfy"
fork. Two ways out:

- **A. SYSROOT=ROOT (self-hosting stage1):** make resolution pull the full
  DEPEND closure into ROOT *and* topologically order it (glibc first). Bigger
  resolver change; yields a more self-consistent root (the minimal hand-built
  stage1 already showed this works — binaries linked ROOT libs).
- **B. SYSROOT=/ (catalyst model):** for a native `--root --config-root /` build,
  set SYSROOT to the seed `/` so DEPEND is host-satisfied for the *build* (the
  pre-flight then passes via the host VDB). Binaries link seed libs → not
  self-consistent → needs a stage2 rebuild-in-chroot, exactly as catalyst does.
  Smaller change, matches the canonical stage1.

NOTE: em currently sets `SYSROOT=ROOT` for this case (shell.rs: build_sysroot
None → EROOT), which is why glibc builds against `ROOT/usr/include` (so the
minimal set needed os-headers built into ROOT first). Path B would flip that.

### DECISION (2026-06-25): SYSROOT=ROOT, break the loop like crossdev

Chose **path A (SYSROOT=ROOT, self-hosting)**. Key user insight: a native
self-hosting stage1 into ROOT is **near-equivalent to the crossdev toolchain
bootstrap** — same circular `glibc ↔ gcc` cycle (gcc needs a libc, libc needs a
compiler), broken the **same staged way**. So reuse the crossdev machinery rather
than inventing a parallel one. The two should converge: crossdev = `CHOST≠CBUILD`
into `/usr/<chost>`; native stage1 = `CHOST==CBUILD` into `--root`.

Implementation direction:
- Reuse `crossdev::stages` (`StagePlan`/`StageStep`, the `GCC_DISABLE_STAGE{1,2}`
  USE overrides, `--nodeps` headers-only steps): binutils → os-headers →
  libc-headers (`headers-only`, `--nodeps`) → gcc-stage1 (no-cxx/no-libc USE) →
  libc → gcc-stage2. Generalize it from `cross-<tuple>/*` atoms to the native
  `sys-devel/{binutils,gcc}` / `sys-libs/glibc` when `CHOST==CBUILD`. The
  stage1-vs-stage2 gcc behaviour is auto-detected by toolchain.eclass from
  whether libc is present, exactly as in crossdev — so the same step USE flags
  apply.
- The staged toolchain breaks the cycle and forces glibc/headers **before**
  gcc/libxcrypt in ROOT, which is exactly what the pre-flight (pos: glibc@103
  after libxcrypt@99/gcc@100) was failing on.
- After the toolchain stage, the rest of `packages.build` builds against the
  in-ROOT toolchain in topological order (the solver handles that part once the
  cycle is pre-broken).

So: factor the crossdev `setup()` staged-bootstrap so it can target a native
`--root` (a `em` stage1 entry point), reusing `stages.rs`. Cross and native then
share one bootstrap driver.

### LANDED (2026-06-25): staged-bootstrap generalized + `em stage1` entry point

The cross-vs-native split is now one typed value, and the two share a driver:

- **`stages::BootstrapKind`** (`{Cross(CrossTarget), Native}`) is the single
  decision point for "build a toolchain into a fresh root" (addresses the
  `cross-support-self-review.md` "no single owner" smell for this slice). The
  ordered step *sequence* is shared; only atom naming differs — cross rewrites
  the category to `cross-<tuple>`, native keeps the real `::gentoo` category.
  `toolchain_plan(&BootstrapKind)` replaces `toolchain_plan(&CrossTarget)`.
- **Shared driver `run_staged`** (`crossdev/mod.rs`): the per-step loop +
  headers + `emerge_atoms`, with a `post_step` hook. `--setup` (cross) passes
  `post_step_cross` (activate `<CTARGET>-*` wrappers + ABI osdirs); native
  passes a no-op. Behaviour-preserving for cross (same `init_target` guard,
  same steps, same activation).
- **`em stage1 [atoms]`** (`Applet::Stage1` → `crossdev::stage1`): the native
  twin of `em crossdev --setup`. Requires `--root <dir>` (bails on `/`). Runs
  the native staged plan (binutils → headers → libc-headers `--nodeps` →
  gcc-stage1 → libc → gcc-stage2), then merges any extra atoms topologically
  against the populated ROOT.

**Verified (`-p` against ::gentoo):** all 6 steps resolve with the right
overrides — `kernel headers [headers-only]`, `libc headers [--nodeps
headers-only]`, `gcc-stage1 [-cxx -fortran -go …]`, `gcc-stage2 [-sanitize]`,
every line `to <ROOT>/` (SYSROOT=ROOT confirmed). 95 tests pass; clippy/fmt
clean. `cross-support-self-review.md`'s consolidation items (the `detect().active`
3-axis OR, the `MergeRoot::Target` default) remain open but are not blocked by
this.

**Remaining to a *running* full stage1 (not yet done):**
1. The staged steps build for real (drop `-p`) — the minimal hand-built stage1
   already built binutils/glibc/bash, so the pieces compile; the staged gcc
   two-stage into `--root` has not been executed end-to-end yet.
2. The pre-flight failures of the *rest* of packages.build (`acct-group/root`,
   `e2fsprogs`, `glibc[-crypt]`, `glibc[cet]` ordering) — the staged bootstrap
   forces glibc/headers first, which should clear the ordering half; the
   "DEPEND never pulled into ROOT" half (acct-group/root, e2fsprogs) may still
   need the emptytree-style DEPEND-into-ROOT closure (Tier-1 item 2).

### First real build attempt (2026-06-25) — reframe + findings

Ran `em --root /var/tmp/stage1-native --config-root / stage1` for real. **It
failed at step 1 (binutils) on the pre-flight, not a build error.** The
investigation clarified the model and surfaced the real blocker.

**Reframe (the key insight — keep this): native stage1 is a *special case of
crossdev*, with `CHOST == CBUILD`.** The crossdev staged bootstrap builds a
**basis** (binutils → headers → gcc → libc) into the sysroot; for native the
sysroot collapses to `ROOT`. That basis then **satisfies** the DEPEND of the
rest of packages.build, which builds against the in-ROOT toolchain. The
committed `em stage1` driver (`8fd2963`) IS this model — do NOT pivot away from
it. (An earlier note in this doc mused that "native has a host seed so there's
no cycle, staging doesn't fit" — that's wrong/misleading: the host seed at
`BROOT=/` provides *build tools*, but the staged basis is what makes the *target
ROOT* self-consistent, exactly as a cross sysroot is. Same machinery.)

**Why the build failed — `debuginfod` pulls the whole closure:**
`em -p sys-devel/binutils` into an empty root pulls **47 packages** (glibc, pam,
curl, openssl…) — genuine transitive deps, not `@system`: `binutils[debuginfod]
→ dev-libs/elfutils[debuginfod] → net-misc/curl → … → sys-libs/glibc`. Cross
`cross-<tuple>/binutils` does **not** do this — so the crossdev staged bootstrap
worked (RISC-V gcc validated) but native step 1 explodes. `debuginfod` is the
specific USE flag (+default in binutils `IUSE="+debuginfod"`) that drags in the
heavy runtime/tooling closure.

**The pre-flight gate that blocks it** (`preflight.rs:33`): each plan entry's
`DEPEND` is checked against `Avail = VDB(base) ∪ VDB(target)`. For a
`--root <empty>` build `base == target == <empty>`, so glibc's `DEPEND` on
`virtual/os-headers` is unsatisfied (it *is* on the host `/`, host-satisfied
during resolution, but absent from the empty ROOT). The os-headers step IS in
the staged plan — but binutils' closure pulls glibc *inside step 1*, before the
staged os-headers step has run.

**The two intertwined problems to solve (both within the staged model):**
1. **Stage USE must match crossdev's.** Native binutils keeps `+debuginfod`;
   cross binutils gets it forced off (the `cross-*` USE set crossdev pins —
   `package.use.force/cross-*`). A native staged step needs the **same minimal
   USE** crossdev uses for each component, or step 1 is not "just binutils" but
   "binutils + its entire native closure." Investigate how crossdev's USE
   forcing applies (the `write_cross_env` / multilib block in `crossdev/mod.rs`,
   plus a likely binutils/gcc `-debuginfod -multitarget …` stage USE set) and
   apply the native analogue in `stages.rs` `Native` steps.
2. **DEPEND-into-ROOT vs host-satisfy (the Tier-1 item-2 fork, still open).**
   Even with minimal stage USE, the rest of packages.build hits DEPENDs the host
   has but the ROOT lacks (`acct-group/root`, `e2fsprogs`). The staged basis
   fixes *ordering* (glibc before libxcrypt/gcc); it does not by itself pull
   every DEPEND into ROOT. Decide: (a) pull the full DEPEND closure into ROOT +
   topologically order it (self-hosting, the "heavier fix" in
   `nonemptytree-bdeps-gap.md`), or (b) the staged basis satisfies most, and
   the residual is bounded. The first real build never got far enough to tell.

**Cleaned up:** `/var/tmp/stage1-native` + logs removed. Build env is ready
(aarch64, 128 cores, 255 GB, host gcc 16.1, distfiles present, MAKEOPTS=-j80).
**Next session:** start with problem 1 (native stage USE = crossdev's component
USE), re-run step 1, then confront problem 2 as it surfaces.

### Problem 1 FIXED (2026-06-26): `-debuginfod` on the native binutils step

Pinned it down empirically (`em -p --root <empty> sys-devel/binutils`):
`debuginfod` (binutils `IUSE="+debuginfod"`) is the lone closure-puller —
**47 packages with it, 7 without** (`elfutils → libarchive → curl → openssl →
gnutls → … → glibc`). gcc by contrast is only 16 in isolation (genuine deps,
shrinks once the staged binutils/glibc are in ROOT), and no other native step
explodes. So crossdev's "component USE set" framing was a red herring — crossdev
keeps `BUSE=""` and never disables debuginfod; cross binutils simply doesn't pull
the closure because it is **host-rooted** (`ROOT=/`, deps already installed).
Native binutils installs into the *empty* ROOT, so the same flag drags the whole
runtime closure in — and the pulled-in glibc was what tripped the os-headers
pre-flight.

Fix (`stages.rs`): the `binutils` step's `use_override` is `["-debuginfod"]` for
`BootstrapKind::Native`, empty for `Cross` (behaviour-preserving; cross keeps the
flag, host-satisfied). Verified: `em stage1 -p` step 1 is now the 7-pkg
internally-consistent closure (zlib → virtual/zlib, gentoo-functions,
binutils-config, xz → zstd → binutils) — no glibc, no os-headers pre-flight trip.
The remaining 6 are genuine binutils build deps and order correctly within the
step.

**Side finding — em ignores `USE="-*"`:** `USE="-* build"` (catalyst's stage1
USE) did NOT collapse the closure (binutils still showed `debuginfod`), while
`USE="-debuginfod"` did. So em's env-USE handling doesn't implement the `-*`
clear-all wildcard. The catalyst `-* build` recipe would therefore not work
through em as-is; the targeted per-step disable is the right lever here regardless.
Filed as a separate gap (low priority — the staged toolchain doesn't need `-*`).

**Next:** run step 1 (and the staged plan) for real into `--root` (drop `-p`),
then hit problem 2 (DEPEND-into-ROOT vs host-satisfy) as the rest of
packages.build surfaces it.

### Real staged run (2026-06-26): steps 1–3 fixed; gcc-stage1 exposed the big one

Ran the full staged plan for real into `/var/tmp/stage1-native`. It walked
further each fix:

- **Step 1 binutils** — built (7 pkgs, `-debuginfod`). Problem 1 confirmed end-to-end.
- **Step 2 kernel headers** — *first* failed step 3 pre-flight `glibc needs:
  virtual/os-headers`: the step built `sys-kernel/linux-headers` (the provider),
  but glibc DEPENDs on the *virtual*, which em host-satisfies and never put in
  ROOT. **Fixed**: native kernel-headers step now merges `virtual/os-headers`
  (pulls linux-headers AND registers the virtual in the ROOT VDB). Step 3 then
  passed.
- **Step 3 libc headers** — glibc (headers-only) merged. (Minor non-fatal noise:
  `failed to redirect to <root>/etc/hosts: No such file` from glibc post-install
  — the ROOT has no `/etc/hosts`; didn't block the merge. Low-priority cleanup.)
- **Step 4 gcc-stage1** — **FAILED at link**: `ld: cannot find crti.o` while
  building `libgcc_s.so`. gcc configured `--enable-shared`.

### THE REFRAME WAS WRONG ABOUT GCC STAGING (2026-06-26)

Root cause is structural, not a bug: **`toolchain.eclass` gates *every* stage1
affordance on `is_crosscompile`** (eclass lines 1404–1505). The
`--without-headers` / `--disable-shared` / headers-only-libc handling all live
inside `if is_crosscompile`; the `else` (native, `CHOST==CBUILD`) branch is
unconditionally `--enable-shared`. So a native gcc built against a *headers-only*
glibc tries to link `libgcc_s.so` and dies on the missing `crti.o`. There is **no
native headers-only/stage1 path** in the eclass.

Consequence: the "native stage1 ≈ crossdev with `CHOST==CBUILD`" equivalence
holds for *ordering* but **breaks at the gcc two-stage split**. The split is a
cross-only artifact — cross needs gcc-stage1 because it has *no compiler for
CTARGET yet*. Native already has one: the **seed compiler at `BROOT=/`** targets
this arch, so it builds **full glibc directly**, and a single **full gcc** then
links against the now-complete ROOT libc. (This is exactly why the earlier
minimal hand-built stage1 — os-headers → glibc → coreutils, *no gcc-stage1* —
worked.) The earlier doc musing that "native has a host seed, no cycle, staging
doesn't fit" was right about *gcc staging* specifically, even though the
ordering-as-staged-basis framing is still correct.

**Fix (`stages.rs`): the native GCC plan is now 4 steps —**
`binutils → os-headers → glibc (full) → gcc (full, GCC_DISABLE only, keeps cxx)`.
The headers-only libc + gcc-stage1 + gcc-stage2 steps are now cross-only
(`if is_crosscompile`-shaped branch in `toolchain_plan`). Cross is unchanged.
`em stage1` driver and `run_staged` are unchanged — only the native plan shape.

**Real run validated (2026-06-26):** the 4-step native plan built into a fresh
`/var/tmp/stage1-native` through baselayout → binutils → os-headers → full glibc,
then gcc. gcc first failed `cannot find crti.o` (next item), then built fully
(gcc-16.1.1 + CHOST wrappers in the ROOT) once the FS skeleton + libcrypt were in
place. Two more fixes landed from this run:

### crti.o / baselayout (2026-06-26, FIXED)

Even with full glibc in ROOT, gcc died `ld: cannot find crti.o` linking
libgcc_s.so. gcc's `-print-multi-os-directory` is `../lib64`, so it resolves CRT
startfiles via `<sysroot>/usr/lib/../lib64`; glibc installs `crti.o` into
`usr/lib64`, but a from-scratch ROOT has no `usr/lib` dir for the `..` to resolve
through. `baselayout` provides that skeleton (`dir /usr/lib`, `dir /lib`) — the
earlier minimal stage1 worked only because it included baselayout. **Fix:** native
plan merges `sys-apps/baselayout` (USE=build) first (5 steps now), as catalyst does.

### Problem 2 RESOLVED — and it was NOT virtuals (2026-06-26)

The gcc step then failed the **pre-flight** `gcc needs: virtual/libcrypt`. Long
investigation (Luca pushed back hard: "we shouldn't treat virtual in special
ways") — and he was right. The chain:

- gcc's DEPEND virtuals: libcrypt, libiconv, libintl, zlib. Three of the four are
  **pulled** into ROOT; only `virtual/libcrypt` is dropped. So NOT blanket virtual
  special-casing.
- The discriminator: `virtual/libcrypt` is gcc's **only DEPEND-only** dep (not
  also in RDEPEND). gmp/mpfr/mpc/zlib/libintl/libiconv are all in gcc RDEPEND →
  never eligible for the DEPEND trim → pulled. libcrypt is DEPEND-only → trimmed.
- Root cause (`depgraph/mod.rs`): the host-config-stage DEPEND trim
  (`trim_sysroot_satisfied_depend`) ran with `roots.sysroot()` = the **config
  root `/`**, not the build sysroot. em builds a from-scratch offset with
  `SYSROOT = ROOT` (base == target → `build_sysroot()` is `None`), so DEPEND must
  be satisfied in the ROOT. The trim checked the host and dropped any host-installed
  DEPEND-only dep. libcrypt (provider libxcrypt, host-installed) was just the lone
  casualty in the toolchain.
- **Fix (`e4ceba0`):** pass `build_sysroot().or(target)` to the trim — no-op for a
  from-scratch ROOT, only `--prefix` (base != target) trims against a real build
  sysroot. gcc -p now pulls virtual/libcrypt + libxcrypt; **@system parity
  unchanged (181 == 181)**.
- **Cleanup (`55a0b5e`):** removed the dead `is_virtual()` skips in the dep trims
  (proven dead — removing them changed nothing; the sysroot fix is what mattered).
  Virtuals are now treated as ordinary packages there. [[no-slop-comments]]

This was the long-feared "DEPEND-into-ROOT vs host-satisfy" fork — and it turned
out to be a one-line root conflation, not a deep resolver redesign or a path-A/B
schism. SYSROOT=ROOT (path A) just needed the trim to agree with the shell.

**Native stage1 toolchain: DONE.** baselayout → binutils → os-headers → glibc →
gcc all build into a fresh `--root`, no manual steps. Remaining (lower priority):
the rest of `packages.build` beyond the toolchain (acct-group/root, e2fsprogs,
util-linux ordering) — re-test now that DEPEND-into-ROOT is fixed; the glibc
post-install `/etc/hosts` redirect noise (cosmetic).
