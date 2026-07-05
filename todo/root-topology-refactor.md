# Root topology refactor — tracked tasks

Design doc: [`docs/root-topology.md`](../docs/root-topology.md). This file
tracks the implementation work it implies. Status: 🔴 not started · 🟡 partial · ✅ done.

## Why

The cross/stage session exposed structural debt: `Roots` is a flat bag of five
`Option<PathBuf>` fields, and `host_roots`/`base_roots()` is threaded
positionally across 9 files. Three "wrong root at one site" bugs (`c421c95`,
`732aefe`, `0e9b3e0`) and the `host_aliases` invariant violation (`208c818`)
all stem from no type telling callers which root answers which role. The
refactor replaces the bag with a `RootTopology` enum whose variant answers
`satisfaction_root(dep_class)` as a pure function.

## Behaviour changes (correctness, not just types)

These are the divergences between current code and the target model in
`docs/root-topology.md` § "Override semantics". Each is a real behaviour change
to land as part of (or before) the refactor.

- 🔴 **`--root` no longer moves config.** Current `cli.rs:274` does
  `config: config_root.or(root)` — config follows `--root`. Portage `ROOT=`
  parity requires config to stay at `/`. This is a user-visible change for
  anyone relying on the current offset-config default; decide clean-break vs
  compat shim.
- 🔴 **`--local` becomes standalone, not overlay.** Current `cli.rs:261-270`
  sets base=`/` (overlay). The target model sets base=target=`~/.gentoo` (full
  closure, self-contained) so `em --local` and `em --local --cross <T>` work
  on a foreign host. The overlay use case moves entirely to `--prefix`.
- 🔴 **Host-python/host-tool symlinks move from `--local` to `--prefix`.**
  Current `setup.rs:163-166` gates `link_host_pythons`/`link_host_base_tools`
  on `is_local` — exactly backwards. The symlinks are an overlay mechanism
  (borrow host tools because base=host); under standalone `--local`/`--root`
  the prefix must own its python. `--prefix` sets EPREFIX=P (relocatable
  installed tree; ebuilds bake `${EPREFIX}/usr/bin/pythonX.Y` into shebangs),
  so the symlink is the principled way to satisfy those shebangs without
  building a prefix python.
- 🔴 **`--prefix` sets EPREFIX=P.** Currently `--prefix` leaves EPREFIX unset.
  Under the target model `--prefix P` is the relocatable overlay: installed
  scripts shebang to `${EPREFIX}/usr/bin/...`, so EPREFIX=P is required for
  the host-python symlinks (above) to actually fire.

## The variant refactor (structural)

- 🟡 **Replace `Roots` with `RootTopology`** per `docs/root-topology.md`:
  ```rust
  struct RootTopology {
      config: PathBuf,                       // PORTAGE_CONFIGROOT, default /
      config_overlay: Option<PathBuf>,       // --prefix P/etc/portage
      roots: RootSet,                        // Single | Dual | Overlayed
      cross: CrossArch,                      // SameArch | ForeignArch(triples)
  }
  enum RootSet {
      Single { root: PathBuf },
      Dual { broot: PathBuf, target: PathBuf },
      Overlayed { broot: PathBuf, base: PathBuf, target: PathBuf },
  }
  ```
  - 🔴 `RootSet::Single`/`Dual`/`Overlayed` + the `satisfaction_root(class)`
    method (table in `docs/root-topology.md` § "What `satisfaction_root`
    returns"). `IDEPEND` is the one cell needing `self.cross`.
  - 🔴 Constructors from `Cli` flags (the override matrix). Normalize
    `Dual { broot, target }` with `broot == target` to `Single`? — open
    question (see doc).
  - 🔴 Migrate the 9 files currently threading `host_roots: &Roots`
    (`preflight.rs`, `bdepend_avail.rs`, `query/depgraph/{mod,installed,
    bdepend_trim,depend_trim}.rs`, `crossdev/mod.rs`, `main.rs`, …) to ask
    `topology.satisfaction_root(class)` instead. This retires the
    `host_roots`-positional smell.
- 🔴 **Privatize `provider.packages` behind `package_data()`.** Today
  `host_aliases` (`provider/mod.rs:708`) maps `Host`→`Target` identity, and
  every consumer must remember to call the alias-resolving `package_data()`.
  `dependency_graph` forgot (`208c818`). Privatize `packages` so
  `package_data()` is the only accessor — kills the bug class.
- 🟡 **Extract `dep_satisfaction_root(class, merge_root)` table** shared by
  the three solver functions (`cross_target_runtime_deps`/`host_native_deps`/
  `broot_filtered` in `solve.rs`) so they don't drift from `preflight`'s
  routing on the next IDEPEND shift.

## Verification

- 🔴 Re-derive "stage1 complete" from a clean `--jobs 1` run of the 4
  stragglers (bzip2, xz-utils, gettext×2), not the VDB spot check
  (`session-status-2026-07-05-needs-review.md`).
- 🔴 Re-merge `app-alternatives/gpg-1-r3` with current `em`, expect
  `IUSE=nls ssl +reference freepg sequoia` in the VDB. If so, close #36 as
  "already fixed; stale entry" — verified via `regen_only` that current code
  produces correct IUSE (`iuse-vdb-already-fixed.md`).
- 🔴 Cross-check each behaviour change above against `emerge` parity in the
  `crossdev-stages` noiseless sandbox before landing.

## Out of scope (deferred)

- Tier 3 mutable-BROOT bootstrap on a foreign host (`build-environment.md`).
- Zero-config merged sysroot via `fuse-overlayfs`/`overlayfs` (M3).
- `binrepos.conf`, signing/verify, `em maint binpkg` tooling — see `PENDING.md`.
