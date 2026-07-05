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

- 🔴 **`--root` no longer moves config.** Current `cli.rs` does
  `config: config_root.or(root)` — config follows `--root`. Portage `ROOT=`
  parity requires config to stay at `/`. This is a user-visible change for
  anyone relying on the current offset-config default; decide clean-break vs
  compat shim. **Deferred** — tangled with `ensure_self_contained_prefix`'s use
  of `config().is_some()` as a self-contained signal; lands cleanly once the
  `RootTopology` enum exists (Cluster C).
- ✅ **`--local` becomes standalone, not overlay.** Landed in `b3f20c1`.
  `base` goes from None (host) to Some(prefix), so base == target ==
  ~/.gentoo — full closure, self-contained VDB. Live-verified in
  crossdev-stages: `em --local -p bzip2` shows `[N] bzip2` +
  `[N] app-alternatives/bzip2` (full closure; reads the empty prefix VDB,
  not the host's). Previously base=`/` would have hidden both.
- ✅ **Host-python/host-tool symlinks moved from `--local` to `--prefix`.**
  Landed in `b3f20c1`. setup.rs's three-mode split (self-contained /
  standalone / overlay) gates `link_host_pythons`/`link_host_base_tools` on
  `is_overlay` (--prefix), not `is_local`. Live-verified:
  `--local`'s `usr/bin/` is empty; `--prefix`'s has python3.13/3.14/find/xargs
  symlinked to /usr/bin.
- ✅ **`--prefix` sets EPREFIX=P.** Landed in `b3f20c1`. Live-verified:
  `em --prefix /opt/test-prefix dev-python/jinja2` builds and merges clean —
  host python3.14/gpep517/flit-core drive the build (BROOT=host), result lands
  in the prefix VDB (counter=1), host VDB untouched (jinja2 counter stays
  395).
  scripts shebang to `${EPREFIX}/usr/bin/...`, so EPREFIX=P is required for
  the host-python symlinks (above) to actually fire.
- ✅ **Split BROOT from install target under `--prefix`.** Landed in
  `21638aa`. `base_roots()` now returns a BROOT view (merge_root=`/` under
  --prefix), and `roots()` reconstructs the prefix-target view on top. Without
  this, `preflight::check` read BDEPEND from the *prefix's* empty VDB instead
  of the host's, failing the jinja2 build with "not satisfied" even though the
  host had all of gpep517/flit-core/python:3.14. Regression test:
  `prefix_overlay_broot_is_host_not_prefix`.

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

## Live test results (2026-07-05, crossdev-stages aarch64 sandbox)

Cluster A + the BROOT/target split were live-verified end-to-end in the
`crossdev-stages` aarch64-20260618T101350Z sandbox (full isolation, real
stage3, no host contamination):

- ✅ `em setup --local` — "standalone Gentoo-Prefix", empty `usr/bin/` (no
  host-python symlinks).
- ✅ `em setup --prefix /opt/test-prefix` — "ROOT-offset overlay",
  python3.13/3.14/find/xargs symlinked into `${EPREFIX}/usr/bin`.
- ✅ `em --local -p bzip2` → `[N] bzip2` + `[N] app-alternatives/bzip2`
  (standalone full closure; base reads the empty prefix).
- ✅ `em --prefix -p bzip2` → `[R] bzip2` only (overlay delta; base reads host).
- ✅ `em --prefix /opt/test-prefix dev-python/jinja2` — built + merged clean,
  host VDB untouched.
- ✅ `em --prefix /opt/xp crossdev -t riscv64-unknown-linux-gnu --init-target`
  — sysroot at `/opt/xp/usr/<tuple>`, overlay + make.conf routing correct
  (`PKG_CONFIG_SYSROOT_DIR`=sysroot, `BUILD_PKG_CONFIG_LIBDIR`=host).
- ✅ `em --prefix /opt/xp cross-riscv64.../binutils` — built + merged
  (counter=1), cross wrapper layout correct, host VDB untouched.
- ✅ `em --prefix /opt/xp select binutils list/show/set` — fully prefix-aware:
  sees host (aarch64) + prefix (riscv64) profiles, distinguishes them, writes
  selection to prefix's env.d, installs the two-hop wrapper symlinks under the
  prefix. **No code changes needed** — `select/mod.rs:config_portage_dir_for`
  already honours `config_overlay`.

## Open follow-ups (found during live testing)

- 🔴 **MAKEOPTS not parallelising gcc's build.** MAKEOPTS=`-j128` is correctly
  set in the sysroot make.conf and emake.rs reads it from the shell env, but
  the live gcc-stage1 compile ran serial (load avg 1.15 on 128 cores). Need
  instrumentation in emake.rs to log the actual make argv, and a check on
  whether toolchain.eclass's gcc `src_compile` uses `emake` or bare `make`.
  Details: `makeopts-emake-parallelism.md` (memory). Blocks the full cross
  toolchain run being fast.
- 🟡 **Top-level `em -j N`** that also sets MAKEOPTS (when unset) — mirrors
  emerge's `--jobs`. Currently `--jobs` only drives parallel package merges,
  not per-package make parallelism. Small feature.
- 🔴 **Full cross toolchain under `--prefix`** — paused mid gcc-stage1
  (MAKEOPTS bug above). Binutils + kernel-headers + libc-headers merged
  cleanly (counters 2,3,4); gcc-stage1 was compiling when killed. Repro state
  preserved in `/opt/xp`; resume by re-running `em --prefix /opt/xp crossdev
  -t riscv64-unknown-linux-gnu --setup` (cached steps skip).
- 🔴 **Full cross stage1 under `--prefix`** — blocked on the toolchain
  completing; then `em --prefix /opt/xp --cross riscv64... stages --stage1`.

## Verification (outstanding)

- 🔴 Re-derive "stage1 complete" from a clean `--jobs 1` run of the 4
  stragglers (bzip2, xz-utils, gettext×2), not the VDB spot check
  (`session-status-2026-07-05-needs-review.md`).
- 🔴 Re-merge `app-alternatives/gpg-1-r3` with current `em`, expect
  `IUSE=nls ssl +reference freepg sequoia` in the VDB. If so, close #36 as
  "already fixed; stale entry" — verified via `regen_only` that current code
  produces correct IUSE (`iuse-vdb-already-fixed.md`).

## Out of scope (deferred)

- Tier 3 mutable-BROOT bootstrap on a foreign host (`build-environment.md`).
- Zero-config merged sysroot via `fuse-overlayfs`/`overlayfs` (M3).
- `binrepos.conf`, signing/verify, `em maint binpkg` tooling — see `PENDING.md`.
