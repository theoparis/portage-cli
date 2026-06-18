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

## Next steps (candidates, not committed)

1. ~~**BDEPEND under ROOT-offset**~~ — *done*: em treats host-provided BDEPEND
   as satisfied by default (`with_bdeps=false`), matching emerge. `@system`
   parity reached (182 == 182). Remaining stage1 question is the toolchain
   subset below, not BDEPEND routing.
2. **Toolchain-subset build** (`gcc`/`binutils`/`virtual/libc`) — the circular
   set the host normally seeds; expect builder/eclass gaps (the point of the
   exercise).
3. **Complete `--local --setup`** as `--prefix ~/.gentoo` + EPREFIX-in-place
   semantics (structural; mostly already true).

## Proven earlier (context, not this session)

- The firefox closure builds into a `--prefix`/`--local` target (the
  accumulated bashrc recipe in `setup.rs`); this is the baseline that `--root`
  characterization builds on.
