# Derive `cross-<tuple>/<pkg>` on the fly (no overlay symlinks)

STATUS: 🟡 design refined, implementation starting.

## The idea
Instead of `em crossdev --init-target` materialising a `cross-<tuple>` overlay
of per-package symlinks (copies, in the broken case) into `::gentoo`, teach
the resolver to treat `cross-<tuple>/<pkg>` as a derived alias for its real
target package (`cross-riscv64-unknown-linux-gnu/gcc` → `sys-devel/gcc`) at
resolve time. The ebuild, metadata, SLOT, IUSE, deps — all the real package's.
Only the **category** in the CPV is cross; the build-time env (CTARGET/ABI
CFLAGS) still comes from the host `package.env` (f84436a, unchanged).

## Why
- Eliminates the stale-overlay bug class (found live: cross overlay lagged
  behind gentoo, `cross-riscv64/gcc-16.1.1_p20260613` existed in `::gentoo`
  but not the overlay → resolver `NoVersions`).
- Eliminates the absolute-symlink portability bug.
- Eliminates the init-target overlay write step (symlinks, layout.conf,
  repo_name, categories) — less ceremony.
- Matches how `<tuple>-emerge` sees the tree: `cross-<tuple>/*` are virtual
  routing labels; the ebuilds are `::gentoo`'s.

## Single source of truth: `CrossTarget::packages()`

The **only** table of "which real packages map to cross for this target" is
`CrossTarget::packages()` (target.rs:147). It already branches correctly on
`llvm` / `has_kernel` / `libc` (Glibc/Musl/Newlib). Adding musl or elf support
needs **zero** derivation changes — `packages()` adapts, the map follows.

The derivation map is built from it:
```rust
HashMap<Cpn(cross-<tuple>/<pkg>), Cpn(<real-cat>/<pkg>)>
```
populated once per solve when `--cross` is active; empty otherwise (zero
overhead for non-cross solves).

## Where the hook lives: Adapter-level (Option A)

The cli's `Adapter` (the `PackageRepository` impl in `repo.rs`) gains a
`cross_map` field. The solver crate sees cross packages as first-class; it
never knows they're derived.

- `all_packages()`: appends the cross Cpns when `cross_map` is non-empty.
- `versions_for(cross-cpn)`: proxies to the real cpn's versions, rewriting
  each Cpv's category to cross. Metadata (slot/IUSE/deps) untouched. Repo
  identity = `::crossdev` (so `::repo` constraints and routing still work).
- `desired_use` / `slots_for` for cross-cpns: delegate to the real cpn's data.

## Keeping the build plan honest

`toolchain_plan` (stages.rs:166) hardcodes per-step atoms + USE overrides +
the stage1/stage2 gcc split. That build-order logic is genuinely plan-specific
(not set-specific) and stays there. The invariant that keeps them in sync:

**every atom `toolchain_plan` emits must be in `packages()`'s set.**

That's a unit test (`toolchain_plan_atoms_are_all_in_packages_set`), not a
code merge. When musl/elf land, `toolchain_plan` gains branching (unavoidable —
musl may not need the two-stage gcc headers cycle), but the test guarantees the
plan never references a package the derivation can't resolve.

## What `init-target` stops/keeps doing

**Stops:** `write_overlay` (the symlink farm), `ensure_repos_conf` for the
overlay. The cross packages become virtual.

**Keeps:** `write_cross_env` (CTARGET/ABI CFLAGS at host package.env),
`write_sysroot_config` (sysroot make.conf with CHOST/CBUILD),
`write_sysroot_repos_conf` (so the sysroot sees `::gentoo`).

## The merge-path decoupling (the one risk)

When the solver picks `cross-riscv64/gcc-15.2.1`, the merge phase must:
- Find the **real ebuild** (`sys-devel/gcc/gcc-15.2.1.ebuild`) — not via a
  symlink, but by resolving the real cpn the cross cpn was derived from.
- Write the VDB entry under `cross-riscv64/` (so gcc-config/binutils-config
  find it, `emerge -u cross-riscv64/gcc` works). Already the case today —
  the cpv category drives the VDB path.

Need to verify: does `PlannedMerge.ebuild_path` derive from cpv (which would
point at a nonexistent cross path without the overlay) or is it already
decoupled? This is the one thing to check before removing `write_overlay`.

## Implementation order

1. Build `cross_map` in `Adapter` from `globals.cross` + `packages()`.
2. `all_packages` / `versions_for` / `slots_for` / `desired_use` proxying.
3. Invariant test: `toolchain_plan` atoms ⊆ `packages()`.
4. Merge-path: resolve the real ebuild for a cross cpv (decouple ebuild_path).
5. Remove `write_overlay` from `init_target`; keep the rest.
6. Validate: `em --cross riscv64 -p cross-riscv64/gcc` resolves without overlay;
   full `crossdev --setup` + `stages --stage1` end-to-end.

## Related
- `crossdev-target.md` (the crossdev feature design, predates this).
- `cross-support-self-review.md`.
- f84436a (package.env at host), `write_overlay` (crossdev/mod.rs:688).
