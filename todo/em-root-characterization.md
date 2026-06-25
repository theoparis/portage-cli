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
