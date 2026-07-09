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
- ✅ **`--root`'s BROOT is the host, not the offset (portage `ROOT=`
  parity).** The fifth behaviour change, missing from this list until
  2026-07-09: `base_roots()` had `base: R, target: R` for plain `--root R`,
  so `merge_root()` (read as BROOT by `preflight`/`bdepend_avail`/
  `load_host_installed`) was the offset itself — BDEPEND satisfaction
  checked the (usually near-empty) offset VDB instead of the real host's.
  Found live: task #17's `--root .../cross-stage1-riscv64 --cross riscv64...
  systemd-utils` kept failing on `jinja2 found: NO` even though the real
  host already has jinja2 for its own python.
  - **The fix went through two passes.** First pass introduced a `RootSet`
    enum (`Single`/`Dual`/`Overlayed`, matching `docs/root-topology.md`'s
    proposed shape) and made `base_roots()` itself return the host for
    `--root`. That broke a *different* thing: `base_roots()` is also relied
    on as "the outer EROOT, `--cross`-substitution undone" by
    `crossdev/mod.rs`'s `bypass_cross_root` (where crossdev's own
    toolchain-bootstrap packages install) and by `write_cross_env`/
    `write_sysroot_config` (which write config those steps read back) — all
    of which correctly need the *offset* for `--root`, not the host. Caught
    it by re-testing `em --root R --cross T crossdev --init-target`, which
    started hitting a real, *new* permission error (`write_cross_env` trying
    to write `/etc/portage/env/...` — the real host — instead of `R/etc/portage`).
  - **Second pass, landed:** reverted `base_roots()` to its original
    behaviour (still "the outer EROOT", unchanged for every flag) and added
    a new, dedicated `Cli::broot()` — the *only* thing that differs from
    `base_roots()`, and only for plain `--root` (BROOT = host `/` there;
    identical to `base_roots()` for `--prefix`/`--local`, where the two
    already agreed). Repointed the four call sites that actually mean BDEPEND
    satisfaction (`emerge.rs`, `dispatch.rs`'s `equery depgraph`,
    `crossdev::resolve_gcc_version`, `merge/mod.rs`'s `entry_roots` host
    routing) from `base_roots()` to `broot()`; left `bypass_cross_root`/
    `write_cross_env`/`write_sysroot_config`/`activate_toolchain` on
    `base_roots()`, untouched. Regression test: `root_broot_is_host_not_offset`
    (checks `broot()` **and** `base_roots()` diverge correctly for `--root`).
  - **Re-verified end-to-end after the second pass**: `em --root R --cross
    riscv64-unknown-linux-gnu crossdev -t riscv64-unknown-linux-gnu
    --init-target` now completes cleanly, unprivileged, with no `/etc/portage`
    write at all — `write_cross_env` correctly lands in `R/etc/portage`. The
    permission wall was **our own bug** from the first pass, not an inherent
    `--root --cross` limitation — corrected the record here (an earlier
    version of this note wrongly called it expected/by-design).
  - The old self-contained-BROOT-in-an-offset workflow (build everything,
    including BDEPEND tools, into the offset itself — what
    `/var/tmp/cross-stage1-riscv64` was actually doing) still has a home:
    `--local`, parameterized to accept a path (`--local DIR`, was a bare
    bool hardcoded to `~/.gentoo`) instead of plain `--root`.
  - Also found while verifying: the solver's BDEPEND routing genuinely
    differs by scenario, and this is by design, not a bug — `broot_filtered`
    (same-arch native `--root`, no `--cross`) routes an unsatisfied BDEPEND
    to `MergeRoot::Target` (build it into the offset itself); only
    `cross_target_runtime_deps` (true cross-arch, `--cross` with
    `CHOST != CBUILD`) routes it to `MergeRoot::Host`, which is what
    `broot()` now correctly feeds. So this fix's effect is specific to cross
    builds — a same-arch `--root pkg` (no `--cross`) was never affected by
    the BROOT bug in the first place, since that path doesn't consult BROOT
    for BDEPEND routing at all.

- ✅ **`crossdev -t T` doubly-nested the sysroot when a global `--cross T`
  was also set, and `--cross`/`-t` were two separate flags for the same
  concept.** Found while reviewing this arc: `crossdev/mod.rs`'s own
  `sysroot()`/`setup_root()`/`main_repo()`/`ensure_self_contained_prefix()`/
  `ensure_prefix_profile()` (the setup-action helpers) used `globals.roots()`
  — which is *already* `--cross`-substituted to the sysroot when the global
  flag is set — so appending `usr/<tuple>` again doubly-nested it
  (`<EROOT>/usr/T/usr/T`). Reproduced live with matching tuples (not just
  mismatched ones). Fixed by adding `Cli::outer_roots()` (extracted from
  `roots()`'s own "no `--cross`" branch, deduplicating that logic) and
  repointing every setup-only helper to it instead of `roots()`;
  `stage1()`/`profile_stack()`/`resolve_gcc_version` correctly keep `roots()`
  (they genuinely want the sysroot substitution).
  - User pushed back on the follow-up fix (a "reject if `-t` and `--cross`
    disagree" guard): two flags for the same concept that need a mismatch
    check are the smell, not something to validate around. Resolved by
    **removing `crossdev`'s local `-t`/`--target` entirely** and renaming
    the global `--cross` to **`--target`/`-T`** (no clash — `t`/`T` were
    unused everywhere). One flag now serves both roles: `em --target T
    crossdev --init-target` sets T up; `em --target T stages --stage1` (or
    any plain atom build) uses it. `CrossdevArgs.target` is gone;
    `crossdev::run` reads `globals.target` directly. Verified live: `em
    --root R --target T crossdev --init-target` (no local `-t` at all) lays
    down the sysroot at `R/usr/T` correctly, and running with no `--target`
    at all gives a clear error instead of silently guessing.
  - This is a case of the same underlying issue as the enum-migration
    item below, one level up: not just "which of several `Roots`-returning
    methods do I call", but "which of several *flags* mean the same thing".
    Worth keeping in mind during the `RootTopology` migration — check for
    other near-duplicate flag pairs while touching this code, not just
    near-duplicate accessor methods.

## The variant refactor (structural)

- ✅ **`Roots.satisfaction_root(DepClass)` — landed 2026-07-09.** Scoped down
  from the doc's original `RootTopology`/`RootSet`-as-storage proposal to a
  smaller, lower-churn fix with the same payoff: rather than replacing
  `Roots`'s flat-field shape with the enum (and renaming the type), added
  two fields — `broot: Option<Utf8PathBuf>` and `is_cross_arch: bool` — so
  **one** `Roots` value carries BROOT correctly even under an active
  `--target` sysroot substitution (previously `roots()`'s `--target`-active
  branch built a fresh `Roots` with `base = target = sysroot`, silently
  dropping BROOT — *that* was why a second `host_roots: &Roots` had to be
  threaded everywhere). `satisfaction_root(class)` is a small match using
  the table in `docs/root-topology.md` § "What `satisfaction_root` returns":
  `Bdepend` → `broot`; `Idepend` → `broot` if `is_cross_arch` else
  `merge_root()`; `Depend` → `base` when it genuinely differs from
  `merge_root()` (an overlay, e.g. `--prefix`) else `merge_root()`;
  `Rdepend`/`Pdepend` → `merge_root()`. Reused the **existing** canonical
  `portage_atom_pubgrub::DepClass` (`Bdepend`/`Idepend`/`Depend`/`Rdepend`/
  `Pdepend`, already shared by the solver's own dependency graph) instead of
  inventing a second, near-identical enum — caught this mid-implementation
  by the same "don't add something redundant" instinct this whole session
  has been about.
  - Migrated every call site that threaded a `roots`+`host_roots` pair
    purely to answer "where does BDEPEND resolve": `preflight::check` (now
    one `roots` param), `bdepend_avail::Avail::initial_bdepend`,
    `bdepend_trim::TrimCtx` (now one `roots` field), `query/depgraph/mod.rs`'s
    `DepgraphOpts` (dropped `host_roots`), `installed::load_host_installed`,
    `crossdev::resolve_gcc_version`, `dispatch.rs`'s `equery depgraph`,
    `emerge.rs`.
  - **`base_roots()`/`broot()` (the method) were *not* fully retired** —
    caught this correcting the plan mid-implementation: `merge/mod.rs`'s
    `entry_roots` needs a *full* `Roots` for a Host-routed entry (its own
    `config()`/`build_sysroot()`/`eprefix()`, to actually merge the package
    there), not just a satisfaction path — `satisfaction_root` can't replace
    that need, only the path-only call sites above. `broot()` stays, now
    documented as explicitly distinct from `satisfaction_root` (a full
    merge-destination `Roots` vs. a bare VDB-lookup path) rather than one of
    several same-shaped near-duplicates.
  - Regression tests updated to call `.satisfaction_root(DepClass::Bdepend)`
    instead of the old `.broot()`-as-a-path pattern; `Roots::for_test` now
    also sets `broot` so BDEPEND-satisfaction tests still see the same root
    without a separate `host_roots` value. Full workspace fmt/clippy/test
    clean; live-reverified `em --root R --target T crossdev --init-target`
    (single-nested sysroot, unprivileged) and a `--target`-active BDEPEND
    satisfaction path.
  - Did not pursue: the `CrossArch`-as-triples enum, or normalizing
    `Dual{broot,target}` with `broot == target` to `Single` — the `Roots`
    struct's own `is_cross_arch: bool` field covers the one thing the doc's
    `CrossArch` was needed for (the `IDEPEND` cell), and there was no
    `Single`/`Dual` variant distinction to normalize once the fix stayed
    field-based rather than enum-based.
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
