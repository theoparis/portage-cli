# Derive `cross-<tuple>/<pkg>` on the fly (no overlay symlinks)

STATUS: 🔴 not started (design idea, would dissolve the overlay entirely).

## The idea
Instead of `em crossdev --init-target` materialising a `cross-<tuple>` overlay
of per-package symlinks (copies, in the broken case) into `::gentoo`, teach the
resolver to treat `cross-<tuple>/<pkg>` as an alias for its real target package
(e.g. `cross-riscv64-unknown-linux-gnu/gcc` → `sys-devel/gcc`) at resolve time.

A `cross-<tuple>/<pkg>` atom then resolves to the same ebuild as the target,
just with the cross category and the cross-triplet env (CTARGET/CHOST/CBUILD +
per-ABI CFLAGS via package.env, same as today). No overlay to create, sync,
or go stale.

## Why
- Eliminates the stale-overlay class of bugs entirely. Today's failure:
  the cross overlay's gcc copies/symlinks lagged behind the gentoo tree,
  so `cross-riscv64/gcc-16.1.1_p20260613` existed in `::gentoo` but not in
  the overlay → resolver `NoVersions`.
- Eliminates the absolute-symlink portability bug (overlay symlinks anchor at
  host `/var/db/repos/gentoo`, breaking if the prefix moves or a different
  tree is mounted there).
- Eliminates the init-target write step (overlay/metadata/layout.conf, the
  category symlinks, repo_name, etc.) — less ceremony, fewer moving parts.
- Matches how a `<tuple>-emerge` wrapper sees the tree: `cross-<tuple>/*` are
  virtual routing labels, the ebuilds themselves are the real `::gentoo` ones.

## Sketch
- A `cross-<tuple>` category is recognised by the resolver as a derivation
  prefix; `cross-<tuple>/<pkg>` maps to `<real-cat>/<pkg>` (the mapping table
  is the same `target.packages()` already in `CrossTarget`: binutils→sys-devel,
  gcc→sys-devel, glibc→sys-libs, linux-headers→sys-kernel, …).
- The cross category is presented as available only when `--cross <tuple>` (or
  `crossdev` context) is active, so it doesn't pollute normal solves.
- The per-cross env (CTARGET/ABI CFLAGS/multilib) still comes from the host
  `package.env` (already correctly placed by `write_cross_env`, f84436a) — no
  change there.
- The category-name in the merge/vdb stays `cross-<tuple>` (PMS-correct for
  crossdev), so installed package identity is unchanged.

## Open questions
- Does the resolver layer (`portage-atom-pubgrub`) already have an alias hook,
  or does this need a new indirection in `PackageRepository::versions_for` /
  the cli `Adapter`?
- `target.packages()` maps category too (gcc→sys-devel, linux-headers→sys-kernel);
  the derivation must honour that, not just rename the category.
- Backward compat: existing sysroots with the materialised overlay should keep
  working until their next `--init-target`.

## Related
- `crossdev-target.md` (the crossdev feature design, 61K — predates this idea).
- `cross-support-self-review.md`.
- The f84436a package.env + the write_overlay symlink code (crossdev/mod.rs:688).
