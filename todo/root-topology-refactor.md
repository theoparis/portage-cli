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

- ✅ **`--root` config resolution — resolved 2026-07-09, not the way originally
  proposed.** Original framing: "`--root` no longer moves config" (portage
  `ROOT=` parity, `config: config_root.or(root)` → `config: config_root`).
  Attempted, reverted, and replaced with a narrower, correct fix after two
  rounds of live findings:
  - **First attempt**: make `config()` default to host, falling back to the
    offset only when `<offset>/etc/portage/make.profile` already exists
    (real ROOT= parity as the common case, self-contained roots still "just
    work" once bootstrapped). **Broke `em select`'s toolchain-slot lookup**:
    a live test (`current_slot_reads_the_active_gcc_config_profile`) caught
    `select/mod.rs`'s `config_portage_dir_for` — a *second*, independent
    consumer of `roots.config()` beyond `crossdev`'s bootstrap — silently
    falling through to the **real host's** `/etc/env.d/gcc` for a
    freshly-created, not-yet-bootstrapped self-contained root (proven with
    real host state: this dev machine's own `riscv64-unknown-linux-gnu`
    gcc-config slot 16 leaked into a supposedly-isolated tempdir test).
  - **Checked real `eselect` for precedent** (at the user's suggestion):
    `/usr/share/eselect/modules/profile.eselect`'s `get_symlink_location`
    does `local root=${PORTAGE_CONFIGROOT-${EROOT}}` — it only ever honours
    an *explicit* `PORTAGE_CONFIGROOT` (or `EROOT`, which a standalone
    invocation never has set); it never cleverly derives a config root from
    `ROOT` alone. `select/mod.rs`'s own doc comment already said as much
    ("`--config-root`... else `--prefix`/`--local` overlay, else `/`" — no
    mention of `--root`) — the actual code just didn't match its own
    documented intent, a pre-existing bug the first attempt's revert
    happened to expose, not something the first attempt caused.
  - **Landed fix**: `Roots::config()` (the merged, build-facing value used by
    `profile_stack`/`expand_sets`/`repos_conf`/crossdev's own bootstrap) is
    reverted to its original, unconditional `config_root.or(root)` — this is
    `em`'s own deliberate self-contained-bootstrap default (own config, own
    everything), not a portage `ROOT=` gap, and touching it broke more than
    it fixed. New, separate `Roots::config_root_explicit()` — *only*
    `--config-root`, never derived from `--root` — is what `select/mod.rs`'s
    `config_portage_dir_for`/`is_prefix_context_for` now use instead of
    `config()`, matching real eselect: `em --root R select ...` reads the
    **host's** config unless `--config-root R` is also given. New
    `Roots::is_self_contained_root()` (topology-only: no EPREFIX, base ==
    target, not bare host) replaces the old `config().is_some()` proxy in
    `crossdev/mod.rs`'s `ensure_self_contained_prefix`/`ensure_prefix_profile`
    — behaviourally identical to before, just no longer coupled to
    `config()`'s exact definition. New
    `Roots::with_own_config_root_if_self_contained()` covers the *internal*
    orchestration case (`crossdev::activate_toolchain`'s own
    gcc-config/binutils-config slot activation for a root it just
    bootstrapped itself) — it forces its own config root without requiring
    the user to also type `--config-root` on every crossdev invocation,
    exactly mirroring how portage's own `{target}-emerge`/build tooling
    exports `PORTAGE_CONFIGROOT` internally rather than expecting the user to.
  - **`--config-root /` already gives literal portage `ROOT=` parity** for
    anyone who wants config to stay at the host for a plain `--root` build
    (e.g. sharing config with an already-installed host system) — no new
    code needed for that direction, it was already the existing escape hatch.
  - Regression tests: `cli.rs` unaffected (no `config()` behaviour change);
    `select/compiler.rs`'s existing
    `current_slot_reads_the_active_gcc_config_profile` updated to pass
    `--config-root` explicitly (the new correct way to test this), plus a
    new `current_slot_ignores_bare_root_without_explicit_config_root`
    asserting the reverse — bare `--root` must *not* pick up the offset's
    env.d, and the internal `with_own_config_root_if_self_contained()` path
    does. Live-verified end-to-end: `em --root R setup` +
    `em --root R --target T crossdev --init-target` still bootstraps
    `R/etc/portage/{make.conf,make.profile,repos.conf}` correctly
    (unaffected); `em --root R --config-root R select compiler show -t T`
    reads a slot written into `R/etc/env.d/gcc` while `em --root R select
    compiler show -t T` (no `--config-root`) reads the real host's instead.
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

- ✅ **`--prefix`'s unsatisfied BDEPEND now weaves host∪prefix VDB and merges
  into the prefix, never the real host.** Found 2026-07-09 by re-deriving
  the topology from scratch: user's stated model — "if you are in --prefix
  you are supposed to install on the prefix the bdepends, the host vdb is
  weaved in ... what is in the prefix drives, but anything that host
  satisfies is not merged again if not explicitly requested" — didn't match
  the code. `Cli::broot()` (the only caller: `merge/mod.rs`'s
  `entry_roots`, used to physically merge a `MergeRoot::Host`-stamped plan
  entry) returned `root_set().broot()` uniformly — host `/` for both
  `--root` (correct, privileged) and `--prefix` (wrong: an unprivileged
  overlay can't write the real host). Latent, not yet hit live: every
  existing `--prefix` test/run happened to have its BDEPEND already
  satisfied by the host (`"host python3.14/gpep517/flit-core drive the
  build"` in this same file's live-test log below — no rebuild ever fired),
  so the wrong-merge-destination path was never exercised.
  - Fix: `Cli::broot()` now returns `outer_roots()` (merge_root == prefix)
    when `base_roots().is_overlay()`, instead of a host-anchored `Roots`;
    unchanged for `--root`/`--local`/bare. `.broot` (the satisfaction root)
    still resolves to the host either way — only the merge destination
    differs.
  - `Avail::initial_bdepend` (`bdepend_avail.rs`) and `load_host_installed`
    (`query/depgraph/installed.rs`) now additionally read the prefix's own
    VDB under `--prefix` (`roots.is_overlay()`), so a BDEPEND already built
    into the prefix by a previous run counts as satisfied. `load_host_installed`
    reads host first, prefix second — `add_host_installed`'s plain
    `HashMap::insert` then makes the later (prefix) entry win for a package
    present in both, matching "what is in the prefix drives".
  - "Not merged again if not explicitly requested" needed no new code — the
    solver's existing `host_satisfied_on_broot`/`append_unsatisfied_broot`
    (`provider/solve.rs`) already drop a satisfied BDEPEND edge outright,
    and an atom named explicitly on the command line is a separate,
    already-existing root-target path unaffected by this.
  - New regression tests: `cli.rs`'s
    `prefix_overlay_broot_merges_into_prefix_not_host`,
    `bdepend_avail.rs`'s `initial_bdepend_weaves_in_the_prefix_vdb_under_overlay`
    / `initial_bdepend_still_finds_host_only_entry_under_overlay`,
    `installed.rs`'s `load_host_installed_weaves_prefix_over_host_under_overlay`
    / `load_host_installed_still_finds_host_only_entry_under_overlay`,
    `merge/mod.rs`'s `host_entry_installs_into_the_prefix_under_overlay_not_the_host`.
    Added `Roots::for_test_overlay(host, prefix)` (test-only constructor)
    since the existing `for_test` collapses base/target/broot to one path.
  - Live-verified: `em --prefix <dir> setup` then `em --prefix <dir> -p
    dev-python/pip` (a real package with genuine `MergeRoot::Host`-routed
    build-time deps, not just the historically-tested host-already-satisfied
    jinja2 case) shows every single line — Host- and Target-routed alike —
    landing `to <prefix>/`, none on the real host. Confirms both the actual
    merge-destination fix and the sibling display fix below together.
  - **Found live, same pass: the `-p` display was a separate, stale code
    path.** `query/depgraph/root_aware.rs`'s `display_root` hardcoded
    `MergeRoot::Host => "/"` — correct before this fix (when `Cli::broot()`
    always *was* host), now stale: the pretend-mode merge list kept showing
    Host-routed entries as landing on `/` even though the real merge
    (`entry_roots`) now correctly sends them to the prefix. Fixed by adding
    `CrossContext.host_target` (computed once in `root_aware::detect`,
    mirroring `Cli::broot()`'s own `is_overlay()` check) and having
    `display_root` read it instead of a hardcoded path. Caught by actually
    reading live `-p` output line-by-line rather than trusting unit tests
    alone — the unit tests cover `Cli::broot()`/the weave correctly, but
    display formatting is a third, independent piece of code that was never
    exercised by them.
  - **Residual gap closed same day, on request ("low hanging fruit").** The
    combined `em --prefix P --target T` case still showed a `MergeRoot::Host`
    entry landing on `/` in `-p` output, because `CrossContext.host_target`
    was derived from `depgraph()`'s `roots` parameter (`cli.roots()`), whose
    `--target`-active branch always clears `eprefix`/`is_overlay()` — losing
    the very signal `host_target` needs. Fixed by threading the correct value
    in from outside instead of re-deriving it from the (possibly-substituted)
    `roots`: new `DepgraphOpts::host_merge_root: &'a Utf8Path` field, set by
    each of the 3 construction sites (`emerge.rs`, `dispatch.rs`,
    `crossdev::resolve_gcc_version`) from `cli.broot().merge_root()` — the
    same authority `merge/mod.rs`'s `entry_roots` already uses for the real
    merge, unaffected by `--target` substitution since it's derived from
    `base_roots()`. `root_aware::detect` now takes `host_merge_root` as a
    parameter instead of computing it from `roots.is_overlay()`.
    Regression test added (`root_aware.rs`'s
    `host_entry_displays_as_landing_in_the_prefix_even_when_roots_is_target_substituted`)
    using a `--target`-shaped `Roots` with a separately-passed prefix path,
    reproducing exactly the bug this closes. **Live-verified**: `em --prefix
    P --target riscv64-unknown-linux-gnu crossdev --init-target` then `-p
    --with-bdeps sys-apps/systemd-utils` shows the Host-routed build chain
    (dev-lang/python + its own openssl/sqlite/glibc/timezone-data) landing
    `to P/`, while the Target-routed sysroot packages land `to
    P/usr/riscv64-unknown-linux-gnu/` — both correct, distinguishable in one
    `-p` run.

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
- ✅ **Privatize `provider.packages` behind `package_data()` — landed
  2026-07-09.** `host_aliases` (`provider/mod.rs`) maps `Host`→`Target`
  identity, and every consumer must remember to call the alias-resolving
  `package_data()`. `dependency_graph` forgot once already (`208c818`);
  a full sweep found **12 more sites with the identical bug**, all reachable
  via `solution.iter()` (which legitimately yields `Host`-flavored entries
  under `--target`/`--prefix` builds) or public-API arguments:
  - `validate.rs`: `check_use_deps`, `check_repo_constraints`,
    `check_blockers`, `slot_operator_bindings` (6 call sites) — each silently
    skipped validation for a `Host`-routed package's USE-deps/repo-constraint/
    blocker/slot-binding.
  - `provider/post_solve.rs`: `compute_use_flag_requirements` (3 sites) and
    `effective_flag_new` — a `Host`-routed package's USE-flag-requirement
    cascade silently under-computed.
  - `provider/mod.rs`'s public `versions_for_pkg`/`deps_for` — currently
    unused by `portage-cli`, but broken for any future `Host`-flavored caller.
  - Also converted `branch_best_installed` (currently safe — its one caller
    always passes a virtual package — but converted anyway for
    defense-in-depth at zero cost) to the same accessor.
  - Confirmed safe, left untouched: `graph.rs`'s `self.packages.get(dp)` for a
    *virtual* `dp` (virtuals are never aliased — `ensure_host_instances`
    filters `!p.is_virtual()` before creating an alias) — converted to
    `package_data()` anyway purely because the field is now private and this
    site is in a different module; `provider/mod.rs`'s own internal uses
    (`add_installed`, the synthetic solver root insert/remove,
    `deps_reach_installed`'s virtual-guarded lookup) — genuinely not
    alias-sensitive, left as direct field access (same module as the
    declaration).
  - **Fix**: `packages` field changed from `pub(crate)` to fully private (no
    modifier) — a compile-time enforcement, not just convention: `graph.rs`/
    `validate.rs` are sibling modules of `provider`, not descendants, so a raw
    `.packages.get()` there is now a hard compile error, catching exactly the
    7 sites the privatization was meant to catch (confirmed by temporarily
    reverting the field to `pub(crate)` and one call site back to
    `.packages.get()` — it compiled again, proving the enforcement is real,
    not incidental). `post_solve.rs`/`solve.rs` are `provider`'s own
    submodules (private fields stay visible to descendants), so those needed
    manual conversion — not compiler-forced, but done for the same
    correctness reason.
  - New regression test: `validate.rs`'s
    `check_blockers_fires_from_a_host_routed_packages_own_blocker` — a
    `Host`-routed package (an unsatisfied BDEPEND, same `set_cross_active`/
    `set_with_bdeps` setup as `graph.rs`'s existing
    `host_package_bdepend_on_another_host_package_orders_correctly`) declares
    a blocker against a normal Target-side RDEPEND; verified this test
    actually fails without the fix (reverted the field + one call site
    temporarily, confirmed red, restored). Full workspace fmt/clippy/test
    clean (141 passing in `portage-atom-pubgrub`, was 140).
- 🟡 **Extract `dep_satisfaction_root(class, merge_root)` table** shared by
  the three solver functions (`cross_target_runtime_deps`/`host_native_deps`/
  `broot_filtered` in `solve.rs`) so they don't drift from `preflight`'s
  routing on the next IDEPEND shift.
  - **2026-07-09: re-checked, description still accurate** (confirmed via
    `git diff`/`git log` that `solve.rs` hasn't changed since the original
    read). The three functions differ along exactly two axes — which
    `MergeRoot` DEPEND/RDEPEND/PDEPEND get stamped with (`Target`/`Host`/
    unstamped) and which `MergeRoot` an *unsatisfied* BDEPEND/IDEPEND edge
    gets stamped with — so the extraction is a small `DepStampPolicy { runtime_stamp:
    Option<MergeRoot>, broot_unsatisfied: MergeRoot, include_depend: bool,
    include_bdepend: bool }` struct plus one shared body, not a literal
    per-`DepClass` table. Still valid, still low priority.

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

- ✅ **MAKEOPTS not parallelising gcc's build — re-verified 2026-07-09 via a
  real, complete gcc-stage1 + gcc-stage2 build.** Confirmed the sysroot's
  make.conf carries `MAKEOPTS="-j128"` (the earlier `crossdev-sysroot-
  makeopts` fix, still landed and test-guarded) and that `toolchain.eclass`'s
  `gcc_do_make` goes through `emake` (not bare `make`). The full cross
  toolchain bootstrap below (both gcc stages) completed in this session's
  timeframe rather than hanging at a serial compile, which is the real-world
  answer the original "load avg 1.15" observation needed. Not instrumented
  down to an exact parallelism measurement, but no longer an open question
  blocking anything — closing as resolved.
- **Top-level `em -j N` also setting MAKEOPTS — rejected 2026-07-09, not
  pursuing.** Decided against per-package/per-invocation MAKEOPTS
  auto-derivation from `--jobs`; `--jobs` stays scoped to parallel package
  merges only, MAKEOPTS stays purely a make.conf/env concern.
- ✅ **Full cross toolchain under `--prefix` — DONE, completed end-to-end
  2026-07-09**, resumed in a fresh `crossdev-stages` aarch64 sandbox (the old
  `/opt/xp` state from the previous session's host didn't exist on this
  machine). Found and fixed three real bugs and corrected one wrong fix along
  the way (full story below). Final live result: `em --prefix /opt/xp
  --target riscv64-unknown-linux-gnu crossdev --setup --jobs 4 --keep-going`
  completed all 6 steps clean —
  `binutils(1)→linux-headers(2)→glibc-headers(3)→gcc-stage1(4)→glibc(5)→
  gcc-stage2(6)`, ending `>>> cross toolchain riscv64-unknown-linux-gnu ready
  in /opt/xp/usr/riscv64-unknown-linux-gnu` with the compiler activated
  (`Switching cross-compiler to riscv64-unknown-linux-gnu-15 ... [ ok ]`).
  Verified no host contamination: `/opt/xp/var/db/pkg/cross-riscv64-…/`
  correctly holds all 4 packages; the sandbox's real `/var/db/pkg` has zero
  `cross-*` entries. This is the first time this exact combination
  (unprivileged `--prefix` overlay + a genuine foreign-arch crossdev
  toolchain bootstrap) has completed successfully.
  - ✅ **Bug 1 — `bypass_cross_root` regression, the real root cause.**
    `em --prefix P --target T crossdev --setup` failed step 1 (binutils) with
    a 47-package DEPEND explosion tripping the os-headers preflight, then
    (once superficially "fixed") with `gcc: error: unknown value 'rv64gc' for
    '-march'`. Root cause: the `--cross`/`-t` -> `--target` unification
    earlier this same session (`bcde18a`) made `crossdev --setup` always run
    with the global `--target` flag active — but `crossdev::setup`'s own
    `run_staged` call still passed `bypass_cross_root: false` (harmless
    *before* the unification, since the tuple used to arrive via crossdev's
    own separate `-t` flag, which never touched `globals.target`). After the
    unification this silently made every toolchain-bootstrap step resolve
    against the *sysroot* (`cli.roots()`) instead of the outer EROOT
    (`cli.base_roots()`) — so `cross-<tuple>/binutils`, a host-arch tool,
    read the sysroot's target-arch make.conf (`CHOST=riscv64`,
    `CFLAGS=-march=rv64gc`) to compile itself, and its DEPEND closure
    (including `debuginfod`'s elfutils/curl/glibc chain) was checked against
    the empty sysroot instead of the host that actually satisfies it. Fixed:
    `crossdev::setup`'s `run_staged` call now passes `bypass_cross_root: true`.
    This is a **regression from earlier in this same session**, not a
    pre-existing bug — never caught because `--init-target` (the only
    crossdev operation live-tested right after the unification) doesn't reach
    `run_staged` at all.
  - ⚠️ **False fix, corrected on the user's pushback.** Before finding bug 1,
    the os-headers explosion looked like it needed `binutils`'s `debuginfod`
    USE flag force-dropped unconditionally (previously only dropped for
    `is_self_contained_bootstrap`). The user flagged this immediately
    ("smells a lot" / "you are tapering around") — rightly: once bug 1 was
    actually fixed, a live `-p` preview confirmed `debuginfod` can stay **on**
    (binutils shows `[ebuild R]` alone, no explosion) because `binutils`'s
    DEPEND now correctly routes to the host, which already satisfies the
    whole closure. Reverted the debuginfod change back to its original
    `is_self_contained_bootstrap`-gated form (and the two tests with it) —
    the real fix was `bypass_cross_root` alone. Lesson: a "fix" that makes a
    symptom go away isn't verified until you check whether a more targeted
    fix (the actual root cause) makes the workaround unnecessary.
  - ✅ **Bug 2 — found and fixed, the actual remaining blocker.** Step 3
    (`libc headers`) failed: `checking installed Linux kernel header
    files... missing or too old!` even though step 2 (`linux-headers`)
    reported a clean merge. Extensive live tracing (temporary `eprintln!`
    instrumentation in `ebuild.rs`, since reverted) confirmed `CTARGET`/
    `CHOST` were correctly different in the build shell, ruling out the
    package.env/CTARGET theory and a suspected `brush`-interpreter
    variable-scoping issue. **Independent review by a second model (Fable,
    at the user's request — "switch the investigation to fable and have a
    second look at the changes you made") found the real cause in ~25
    minutes by reading the VDB directly**: `bypass_cross_root: true` (bug 1's
    fix) routes through `cli.base_roots()`, but under `--prefix`,
    `base_roots()`'s `merge_root()` is deliberately the **BROOT** view (host
    `/`, `target: None` — see its own doc comment) — not the outer EROOT
    `bypass_cross_root` actually needs. Every toolchain step was merging onto
    the *sandbox's real host root* instead of `/opt/xp` — confirmed via the
    VDB (`cross-riscv64-unknown-linux-gnu/linux-headers` registered under the
    sandbox's real `/var/db/pkg`, not `/opt/xp/var/db/pkg`) and `walk_image`
    stripping the `P` subtree out of `${ED}` (since `eprefix=Some(P)` makes
    `ED = D + P`, so a merge rooted at `/` writes real files at `D/P/...`
    while `${ED}` search only ever looks under `D/`). Binutils "worked" only
    by accident (its real-arch binaries landing on the real `/usr/bin` is
    harmless to *notice*, unlike headers going missing from the sysroot).
    **Fixed**: every `bypass_cross_root`-adjacent call site changed from
    `base_roots()` to `outer_roots()` — `emerge.rs`'s own `roots` selection,
    plus `crossdev/mod.rs`'s `activate_toolchain`, `maybe_weave_in_gcc_update`,
    and `write_sysroot_config` (three more call sites with the identical bug,
    found by grepping for `base_roots()` after the first fix). `--root`
    (where `outer_roots() == base_roots()`, no `eprefix`) is a no-op change;
    `write_cross_env` already used `config_overlay()` rather than
    `merge_root()` and needed no change. Live-verified end-to-end (see the
    ✅ summary above) — this was the last blocker.
  - Sandbox: destroyed the ad-hoc `em-item6-9-test` sandbox (it had gotten
    contaminated by bug 2 merging onto its real root) and switched to the
    pre-existing `~/.cache/crossdev-stages/sandboxes/aarch64-20260618T101350Z`
    — already prepared from the 2026-07-05 session, so no re-sync needed;
    wiped its stale `/opt/xp` before retesting. `em` binary copied to
    `/opt/em/em` inside it, driven via `crossdev-stages sandbox run --name
    aarch64-20260618T101350Z "..."`.
- 🟡 **Full cross stage1 under `--prefix` — plan now computes, found and
  fixed a fourth real bug (host-arch cross-tool keyword acceptance), real
  build not yet run.** Attempting `em --prefix /opt/xp --target
  riscv64-unknown-linux-gnu stages --stage1` hit `maybe_weave_in_gcc_update`'s
  gcc-refresh sub-resolve failing outright: `resolution failed: __internal__/
  root 0 depends on cross-riscv64-unknown-linux-gnu/gcc 16.1.1_p20260613`.
  - **Root cause, precisely isolated**: `query/depgraph/mod.rs`'s
    `accept_arch = cross.target_arch().unwrap_or(arch)` is one blanket arch
    for the *entire* resolve. `cross-<tuple>/{binutils,gcc,
    clang-crossdev-wrappers}` are host-arch tools (they run *on* the build
    host; only their *output* targets the CTARGET), but get keyword-checked
    against whichever arch happens to be active for the invocation — the
    sysroot's target arch under `--target` (whose own generated make.conf
    happens to permissively accept `"{arch} ~{arch}"`, masking the problem),
    the bare host's real arch otherwise (typically stable-keywords-only, so
    a not-yet-stable gcc version is genuinely rejected there). Confirmed by
    isolating the exact repro: `em --prefix P -p '=cross-.../gcc-16.1.1...'`
    (no `--target`) failed the same way; the identical atom with `--target`
    also set succeeded — same package, same version, different result,
    purely from which arch axis was active.
  - Initially misdiagnosed as a generic "autounmask doesn't cover a masked
    *top-level/pinned* target atom" gap (traced `find_autounmask_candidates`,
    `query/depgraph/repo.rs:1037`, to confirm it only computes suggestions
    from `dropped_deps` — droppable dependency *edges* with an alternative,
    not a hard top-level atom pin with none) — real gap, but not what this
    needed; corrected after re-reading `accept_arch`'s own construction.
  - **Fix, much smaller than either of the above**: real portage's `**`
    keyword token ("accept regardless of keywords") already exists in `em`
    (`AcceptToken::Any`/`ArchAccept.any`, `query/depgraph/repo.rs:48,95-99`)
    and is arch-agnostic by construction. `write_cross_env`
    (`crossdev/mod.rs`) now also writes a `package.accept_keywords` entry
    (`{category}/{pkg} **`) for the host-arch tools (`!is_target_package`),
    reusing the exact same directory-of-files convention already used for
    `package.env` there. No solver/AcceptKeywords changes needed at all —
    this was the user's own suggested direction ("use the autounmask
    machinery we have... crossdev hacks in sad ways the right masks in a
    very ad hoc way") once the real per-package mechanism was found instead
    of a per-resolve dual-config-read.
  - Live-verified: after re-running `crossdev --init-target` (regenerates
    the config), `em --prefix P -p '=cross-.../gcc-16.1.1_p20260613'` (no
    `--target`) now resolves; `em --prefix P --target T stages --stage1 -p`
    now computes a full, real stage1 plan (hundreds of packages) instead of
    hard-failing. Remaining output at that point is a normal `-p` REQUIRED_USE
    advisory (`sys-apps/util-linux`'s `su? ( pam )`, `net-misc/curl`'s `quic?
    (...)`) — expected `-p` behaviour (matches emerge's own "changes are
    necessary" preview semantics), not a bug.
  - Not yet attempted: the actual (non-`-p`) stage1 build — a long,
    real compile, natural next step but its own separate pass.
- ✅ **Host-arch classification made robust, not a hardcoded name list.**
  The `**`-keyword fix above shipped with `is_target_package(pkg: &str)
  -> bool` — `!matches!(pkg, "binutils" | "gcc" | "clang-crossdev-wrappers")`
  — a name list kept separately from `CrossTarget::packages()`, the actual
  source of the package set. User flagged this directly as "a crossdev
  limitation we should avoid" (relevant if `--ex-pkg`ing the clang wrappers
  or a future `rust-std` for LLVM+Rust cross builds) — and it was already a
  *live* bug, not just a future risk: `("dev-debug", "gdb")` is in the
  GCC-mode package list but isn't in `is_target_package`'s exclusion set, so
  `gdb` (which runs on the host to debug target binaries, exactly like
  binutils/gcc) was silently getting the *target* multilib env block and no
  `**` keyword entry.
  - **Fix**: `CrossTarget::packages()` now returns
    `Vec<(&'static str, &'static str, PackageArch)>` — a new
    `PackageArch { Host, Target }` enum stated at each package's own push
    site in `target.rs`, the single place a cross package is declared.
    `gdb` is now `Host`. Adding a future package (`rust-std`, etc.) forces
    picking `Host`/`Target` right there — no separate list to remember to
    update. `is_target_package` removed entirely; `write_cross_env` reads
    `arch.is_target()` (for `multilib::env_block`'s ABI selection) and
    `arch == PackageArch::Host` (for the `**` keyword entry) straight off
    the tuple instead.
  - All non-classification callers of `.packages()` (`show_target_cfg`,
    `write_alias_repo_conf`, `alias_packages_line`, and their tests) just
    destructure and ignore the third field — no behaviour change for them.
  - Verified: `cargo fmt --check`, `cargo clippy --workspace --exclude
    portage-bench --tests -- -D warnings`, `cargo test --workspace --exclude
    portage-bench` all clean; `crossdev::target::tests::
    riscv_gnu_is_glibc_with_kernel` now asserts `gdb` is `PackageArch::Host`
    directly (previously only checked glibc/linux-headers presence).
  - While fixing this, found and fixed two **pre-existing, unrelated**
    clippy breaks already on `master` before this fix (from the earlier
    `--prefix` BDEPEND-weave commit): `clippy::err_expect` in
    `write_alias_repo_conf_rejects_a_missing_source_package`, and
    `clippy::items_after_test_module` from `merge/mod.rs`'s
    `entry_roots_tests` sitting before later non-test items — moved that
    module to the end of the file, no logic change.
  - Live-verified in the pre-existing `aarch64-20260618T101350Z` sandbox
    (before the correction below was applied): with the *old* binary still
    copied in, `package.accept_keywords/cross-riscv64-unknown-linux-gnu` had
    only `binutils`/`gcc` (`**`), no `gdb` line. After copying in the fixed
    binary and re-running `crossdev --init-target`, `gdb **` appeared and
    `gdb.conf` carried the host ABI matching `binutils.conf`/`gcc.conf`. This
    confirmed the classification mechanism itself works — but see below,
    the premise that `gdb` belongs in the base set at all was wrong.
  - **Correction (same session, user caught it): `gdb` shouldn't have been
    classified as `Host` — it shouldn't be in the base package set at all.**
    Asked "why gdb entered the list though?", then "gdb is optional in
    crossdev. did you get confused?" — yes. Checked `/usr/bin/crossdev`
    directly (`ex_gdb() { [[ ${EX_GDB} == "yes" ]]; }`, `--ex-gdb` sets
    `EX_GDB=yes`, `ex_gdb && doemerge ${DPKG}` at the very end): real
    crossdev only builds a cross gdb when `--ex-gdb` is explicitly passed —
    it's an opt-in "extra" alongside `--ex-pkg`, not part of the base
    binutils→headers→gcc→libc toolchain. `em`'s own design notes
    (`todo/crossdev-target.md:358`) already documented this correctly:
    `"Extra (after stages): --ex-gcc→$GPKG-extra, --ex-gdb→$DPKG, --ex-pkg
    X→doemerge X"`. `em` has no `--ex-gdb`/`--ex-pkg` mechanism yet, so
    `dev-debug/gdb` being unconditionally in `CrossTarget::packages()`'s
    GCC-mode list (since the very first commit introducing it, `a3c7727`)
    was simply a mistake, not a deliberate "always build it" choice — it
    had nothing to opt out into. Fix: removed the `gdb` push from
    `packages()` entirely (not reclassified — removed); updated the one test
    that asserted its presence to assert its *absence* instead. Re-verified
    `cargo fmt --check`/clippy/full test suite clean, and the live sandbox
    toolchain preview (`--show-target-cfg`) no longer lists a `.../gdb` row.
    The `PackageArch` classification refactor itself (previous bullet)
    stands on its own merits independent of this correction — `binutils`/
    `gcc`/`clang-crossdev-wrappers` are still `Host`, `linux-headers`/libc/
    LLVM runtimes still `Target`, still declared at each push site instead
    of a separate name list.
  - **Correction #2 (same thread, user caught it again): `--ex-pkg` is
    already fully supported — there is no missing mechanism, and my
    "confirmed live bug" claim below this line in an earlier edit was a
    self-inflicted false alarm.** I tested `em --prefix P --target T -p
    cross-riscv64-unknown-linux-gnu/gdb` (with `--target` *and* an explicit
    `cross-<tuple>` atom together) and saw it merge into the sysroot
    instead of the host, and went and read `repo::target_package`/
    `solve.rs`/`host_copies.rs` hunting for a classification gap. User:
    "for cross-{riscv64-unknown-linux-gnu} you simply do `em -p` if you
    pass a `--target T` it means that you are trying to set CHOST=T
    CTARGET=riscv64-unknown-linux-gnu, and it would be a quite different
    thing, isn't it?" — yes. The `cross-<tuple>` **category already fully
    identifies the cross target**; naming `cross-riscv64-unknown-linux-gnu/
    gdb` needs no `--target` at all. `--target T` is a separate, session-
    wide concern (dual-root CHOST/CTARGET context for resolving *ordinary*
    non-cross-category packages against the target sysroot, e.g. for `em
    stages --stage1`) — combining both for a directly-named cross-category
    atom is a redundant/conflicting invocation, not the real usage shape.
    Re-tested without `--target`: `em --prefix /opt/xp -p
    cross-riscv64-unknown-linux-gnu/gdb` → `cross-riscv64-unknown-linux-gnu/
    gdb-9999 ... to /opt/xp/` — correctly lands in the prefix, no sysroot
    involved. *(Superseded below — this test was unknowingly run against a
    stale alias file that still declared `gdb`; see the staleness bug and
    correction #3.)*
  - **Correction #3: `--ex-pkg` IS a real, currently-missing feature —
    "no --ex-pkg work needed" above was also wrong**, caught by re-testing
    properly after fixing the staleness bug below. Once the sandbox's alias
    file was actually regenerated fresh (deleting the stale one, since
    `write_if_absent` never refreshes it — see next item), `em --prefix
    /opt/xp -p cross-riscv64-unknown-linux-gnu/gdb` correctly failed with
    `no ebuilds found` — `gdb` is no longer in `CrossTarget::packages()`'s
    fixed compile-time list, so the alias declaration no longer exposes it,
    and there is no CLI mechanism to add it (or any other extra) back. User:
    "so --ex-pkg it is a concern for the __crossdev__ applet and in our
    case it means adding an entry to the alias map. And --ex-pkg packages
    need to be aware of ctarget to be meaningful." Confirmed against
    `/usr/bin/crossdev` directly: `for_each_extra_pkg set_portage X` (line
    1675) calls `set_portage` with `l=X`, and `set_env`'s `case ${l} in K|L)
    ... ;; *) ... ;; esac` (line 1483) means `X` always takes the **host**
    ABI branch — every `--ex-pkg` extra gets host-ABI env, unconditionally,
    same as binutils/gcc/gdb. So `--ex-pkg` in `em`'s model means: (1) add
    the atom to the alias-packages set (so `cross-<tuple>/<pkg>` resolves
    at all), (2) write its `package.env` entry via the same `write_cross_env`
    mechanism, always on the host-ABI branch. `--ex-gdb` is pure sugar for
    `--ex-pkg dev-debug/gdb` — no separate code path. **Not yet implemented
    — this is the next concrete task**, tracked below.
  - **Staleness bug found and fixed while re-testing**: `write_alias_repo_conf`
    (and `write_sysroot_repos_conf`'s own copy of the same alias entry) wrote
    via `write_if_absent` (`util.rs:9`), which never overwrites an existing
    file regardless of content — so the drift-detection check above it was
    dead code; a stale alias from a prior run (e.g. still declaring `gdb`
    after it was removed from `packages()`) was never refreshed by a later
    `--init-target`, only by deleting the file by hand. Fixed: extracted
    `write_or_refresh_alias_conf(file, category, packages_line)`, used by
    both call sites — absent → write fresh; present and matching → no-op;
    present, ours (`alias-target =` key) but stale → overwrite; present,
    foreign (no `alias-target =` key, e.g. a real crossdev/eselect-managed
    physical overlay) → never touch. Also fixed `write_sysroot_config`'s
    `make.conf`, which had the identical bug (`write_if_absent`, content
    derived from `target`/`outer_root`, both able to legitimately change
    across `--init-target` re-runs, e.g. a different `--prefix`) — switched
    to an unconditional `std::fs::write` like `write_cross_env` already
    correctly does; no foreign-entry concern there (entirely em-managed,
    unlike the host's real make.conf). Left `ensure_self_contained_prefix`'s
    and `write_sysroot_repos_conf`'s `gentoo.conf` (bare `location =
    <host-repo-path>`, no per-target content, no real drift scenario) as
    `write_if_absent`. New regression tests:
    `write_alias_repo_conf_refreshes_a_stale_own_entry`,
    `write_alias_repo_conf_never_touches_a_foreign_entry`. Full workspace
    `fmt`/clippy/test clean.
- ✅ **`--init-target` now honours `-p`/`-a` like every other mutating `em`
  path.** Before this, `init_target()` (the standalone `--init-target` flag's
  entry point, `crossdev/mod.rs:132`) wrote every file unconditionally and
  immediately — no preview, no confirmation, unlike `-p`/`-a` everywhere else
  in `em`. User: "let's try to understand better, we can weave -p and -a to
  cover the regeneration of make.conf and package.env and such."
  - **Design**: new `crossdev/config_plan.rs` module. A `ConfigEntry` enum
    (`File` — always regenerated, em owns the content; `CreateOnly` — write
    only if absent, e.g. a bare `location =` string with no real drift
    scenario; `Alias` — the `[crossdev]` alias entry's existing absent/
    match/stale-own/foreign logic from the previous fix, generalised;
    `Dir`; `Symlink`) separates *computing desired state* (no I/O beyond
    validation) from *diffing against disk* from *applying*. `config_plan::
    apply(entries, globals)` diffs the whole batch, then: `-p` prints what
    would change and writes nothing; `-a` prints the same and confirms once
    (`confirm_config_write`, mirroring `merge/mod.rs`'s `confirm_merge`)
    before writing; otherwise applies directly. Returns an `Outcome` so
    `init_target` only prints its "cross target ready" summary when
    something was actually applied (or there was nothing to do) — not after
    a preview or a decline.
  - Every existing write-helper (`write_alias_repo_conf` →
    `alias_repo_conf_entry`, `write_sysroot_config` → `sysroot_config_entries`,
    `write_sysroot_repos_conf` → `sysroot_repos_conf_entries`,
    `write_cross_env` → `cross_env_entries`, `ensure_prefix_profile` →
    `prefix_profile_entries`) now *collects* `ConfigEntry` values instead of
    writing directly; `init_target` gathers them all and makes one
    `config_plan::apply` call. `setup::bootstrap` (a separate, already
    pretend-aware subsystem — EPREFIX skeleton/bashrc) stays outside the
    plan, now gated by an explicit `!globals.pretend` inline in
    `init_target` (previously implicit via the whole-function `if
    !globals.pretend { init_target(...) }` gate at `setup()`'s call site,
    which is now removed since `init_target` handles pretend internally —
    meaning `em crossdev --setup -p` now *also* previews the config-plan
    changes, which it previously skipped silently).
  - `em toolchain --setup`'s native path keeps an eager, non-interactive
    `ensure_self_contained_prefix(globals) -> Result<Utf8PathBuf>` wrapper
    (bootstrap + `config_plan::apply_now`, no diff/preview/confirm) since
    that call site is already externally gated by `!globals.pretend` and
    doesn't need its own preview.
  - New tests in `config_plan.rs`: `pretend_writes_nothing`,
    `plain_run_applies_directly`, `no_change_is_reported_as_nothing_to_apply`,
    `create_only_never_overwrites_existing_content`,
    `dir_entry_creates_a_missing_directory`,
    `alias_entry_never_touches_a_foreign_file`. Existing
    `write_alias_repo_conf_*` tests kept via a test-only compatibility shim
    (`alias_repo_conf_entry` + `config_plan::apply_now`).
  - Live-verified in the `aarch64-20260618T101350Z` sandbox with an injected
    stale alias: `-p` printed `update .../crossdev.conf` and left the file
    untouched; `-a` piped `n` printed the same preview + `>>> Quitting.` and
    left it untouched; `-a` piped `y` applied it and printed the normal
    "ready" summary; a further plain re-run (nothing to change) printed only
    the "ready" summary with no `config changes` noise — matches emerge's
    own `-p`/`-a` UX exactly.
  - Full workspace `fmt`/clippy/test clean.

## Verification (outstanding)

- 🔴 **Re-derive "stage1 complete" — accepted 2026-07-09, next up.** From a
  clean `--jobs 1` run of the 4 stragglers (bzip2, xz-utils, gettext×2), not
  the VDB spot check (`session-status-2026-07-05-needs-review.md`).
- 🔴 **Re-merge `app-alternatives/gpg-1-r3` — accepted 2026-07-09, next up.**
  With current `em`, expect `IUSE=nls ssl +reference freepg sequoia` in the
  VDB. If so, close #36 as "already fixed; stale entry" — verified via
  `regen_only` that current code produces correct IUSE
  (`iuse-vdb-already-fixed.md`).

## Out of scope (deferred)

- Tier 3 mutable-BROOT bootstrap on a foreign host (`build-environment.md`).
- Zero-config merged sysroot via `fuse-overlayfs`/`overlayfs` (M3).
- `binrepos.conf`, signing/verify, `em maint binpkg` tooling — see `PENDING.md`.
