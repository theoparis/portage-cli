# `em` stage production (`em stages`) + binhosts

Separating two concepts that the early `em stage1` applet conflated:

- **toolchain** — produce a working compiler + libc in a root. `em toolchain
  --setup` (native, `CHOST==CBUILD`) and `em crossdev --setup` (cross) — they
  share `crossdev::stages` (`BootstrapKind`, `run_staged`). NOT a "stage".
- **stages** — assemble the actual release artifacts (stage1/2/3/4) *using* a
  toolchain (or a seed). This is what `em stages` will own. Not built yet.

`em toolchain --setup` landed (renamed from `em stage1`): builds
`baselayout → binutils → os-headers → glibc → gcc` into `--root`. See
`todo/em-root-characterization.md` for how each step was made to work.

## What catalyst does (native; seed = a stage3) — `~/Sources/catalyst/targets/`

Pipeline **seed → stage1 → stage2 → stage3** (→ stage4):

- **stage1** (`stage1/chroot.sh`): into empty ROOT with the **seed's compiler**:
  1. `emerge --oneshot --nodeps sys-apps/baselayout` (USE=build) — FS skeleton.
  2. `emerge --implicit-system-deps=n --oneshot <packages.build>` with
     `USE="-* build ${BINDIST} …"`, `FEATURES="nodoc noman noinfo"`.
  - `packages.build` is a **profile file** (`profiles/.../packages.build`);
    `build.py` intersects it with the profile `packages` to get versioned atoms.
    It is the *minimal bootstrap subset* of @system; the toolchain is just part
    of it, **built by the seed** — no staged gcc dance. Result is NOT
    self-hosting (links seed libs for anything not yet in ROOT).
- **stage2** (`stage2/chroot.sh`): treat stage1 as `/`; run the Gentoo
  `scripts/bootstrap.sh` — rebuild binutils/gcc/glibc/baselayout with stage1's
  own tools. This is what severs the seed and makes the toolchain
  self-consistent.
- **stage3** (`stage3/chroot.sh`): `emerge -e --update --deep --with-bdeps=y
  @system` — emptytree rebuild of all of @system with the stage2 toolchain. The
  canonical, self-hosting base. (stage4 = stage3 + extra packages/config.)

## What crossdev-stages does (cross; seed = the crossdev toolchain) — `~/Sources/crossdev-stages/cross-stage.sh`

Pipeline **toolchain → stage1 → stage3** — it **skips stage2**:

- **toolchain** (`setup_crossdev`): `crossdev <tuple> --init-target` then the
  real cross compiler build. Explicitly separate from the stages.
- **stage1** (`make`→`install_stage1`): into ROOT with the **cross compiler**:
  `ROOT=$1 USE=build <tuple>-emerge -k -b baselayout`; then `-k -b
  ${STAGE1_PACKAGES}` (the same `packages.build` list, from
  `profiles/default/linux/packages.build`); then `USE=build … -k -b portage`.
- **stage3** (`update`→`update_stage3`): refresh the cross toolchain, then
  `ROOT=$1 <tuple>-emerge -k -e @world` (emptytree rebuild into ROOT), then
  `ldconfig -r`.
- No stage2 — the cross toolchain is already independent (built by crossdev on
  the host), so there is nothing to self-rebuild. **This is the key lesson for
  em**: with our own clean toolchain we can likely go toolchain → stage1 →
  stage3 and skip stage2. BUT our `em toolchain` is built by the *host seed*
  compiler, so a stage2-style `bootstrap.sh` rebuild is the rigorous way to
  guarantee no seed leakage. Decide per-use (fast path vs. provably clean).

## `em stages` — proposed design (NOT built)

Map the above onto em primitives we already have:

| stage | action | em today |
|-------|--------|----------|
| toolchain | `baselayout→binutils→os-headers→glibc→gcc` into ROOT | `em toolchain --setup` ✅ |
| stage1 | `baselayout` (USE=build, --nodeps) + `packages.build` (USE="-* build") into empty ROOT, via a toolchain/seed | `em --root @system`/`packages.build` resolves exist; needs the `packages.build` set + USE wiring |
| stage2 | `bootstrap.sh`-style toolchain self-rebuild (native only) | none — could reuse `em toolchain` *inside* the stage1 root |
| stage3 | `emerge -e @system`/@world — emptytree rebuild | `em --root <r> --emptytree @system` (engine exists) |
| stage4 | stage3 + extra packages/config | `em --root <r> <atoms>` after stage3 |

Open design questions:
1. **`packages.build` ingestion.** Read `profiles/<prof>/packages.build` stacked,
   intersect with `packages` for versions (catalyst's `build.py`). em already
   stacks profile `packages` for `@system`; add `packages.build` reading.
2. **`USE="-* build"` requires em to honour the `-*` wildcard** — it currently
   does NOT (see `todo/em-root-characterization.md` side finding). Either
   implement `-*` in env/profile USE, or drive the minimal USE another way.
   Blocks a faithful stage1.
3. **`--implicit-system-deps=n`** (catalyst) ≈ em's `--with-bdeps=n` +
   not auto-pulling the full @system closure. Confirm the mapping.
4. **CLI shape**: `em stages stage1|stage2|stage3|stage4 --root <dir>` (and a
   `--seed`/`--toolchain` selector?), or `em stages --to stage3`. Probably a
   `stages` subcommand with per-stage actions mirroring catalyst's chroot.sh.
5. **stage2 reuse**: a native stage2 is literally `em toolchain --setup`
   re-run *inside* (chrooted to) the stage1 root. Could share the same plan.

## Binhosts — fast stage3/stage4 assembly (NEW, important)

The slow part of stage3 (`emerge -e @system`) and stage4 (extra packages) is
*compilation*. Catalyst/crossdev-stages already build with `-b`/`-k`
(`--buildpkg`/`--usepkg`): every package is saved as a binpkg and reused. With a
populated **binhost** (a binary package repository), stage3/stage4 become
*download + merge* instead of *compile* — minutes, not hours. This is the lever
for assembling stages quickly, especially across arches.

em status: `em -b/-k/-K/-g/-G` flags exist (buildpkg/usepkg/usepkgonly/
getbinpkg/getbinpkgonly), and `em maint binhost` ("Generate binary package
metadata index"). So the pieces are partly there. Work to do:

1. **Producer: PKGDIR + `Packages` index.** Confirm `em -b` writes binpkgs to
   `PKGDIR` and `em maint binhost` generates a correct `Packages` index
   (the binhost manifest emerge/portage consume). Validate the index format
   against portage's `emaint binhost` / `--regen`. Verify GPKG (the new binpkg
   container) vs XPAK.
2. **Consumer: `--getbinpkg` over a remote `PORTAGE_BINHOST`.** Wire
   `-g/-G` to fetch the remote `Packages` index + binpkgs over http(s), honour
   `BINHOST`/`PORTAGE_BINHOST`, and prefer a binpkg when its USE/version match
   (else build). Today `-k` is local PKGDIR; remote fetch needs the HTTP path.
3. **Binpkg validity / rebuild triggers.** A binpkg is reusable only when
   version + USE + ABI + (sub)slot match the resolved want — reuse the solver's
   `[flag]`/USE-dep machinery (the same one that drives the build/rebuild
   decision) so a stale-USE binpkg is rebuilt, matching `emerge -k`.
4. **Per-arch binhost for stage assembly.** A cross or native stage3 build
   `emerge -e @world` with `-k` against an arch-matched binhost = near-instant
   re-rolls. crossdev-stages does exactly this (`-b -k`). `em stages` should
   default to `--buildpkg` so each run populates the binhost for the next.
5. **Signing / trust.** Portage supports binpkg GPG signing
   (`BINPKG_GPG_SIGNING_*`); a real binhost needs sign + verify. Lower priority
   than getting fetch/produce working, but note it.

Sequence: get producer (1) + local reuse solid, then remote consumer (2/3),
then make `em stages` lean on it (4). Signing (5) last.
