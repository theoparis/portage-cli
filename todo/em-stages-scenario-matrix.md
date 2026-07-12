# `em stages --stage1` scenario matrix — full re-validation across root modes

STATUS: in progress — executing. Using one dedicated sandbox per scenario
(not shared) since `crossdev-stages sandbox setup` is fast/cheap per-name.

## Execution log

- **`crossdev-stages sandbox setup --dry-run` is not actually a dry-run**:
  running it against a new `--name` unpacked the real 1.8G stage3 anyway
  (confirmed: `sandbox list` showed it `state=unpacked` immediately after,
  `du -sh` showed 1.8G). Not our code (crossdev-stages is a sibling repo,
  not fixed here per [[dont-commit-to-sibling-repos]]) — just noting so a
  future `--dry-run` isn't trusted at face value for this tool.
- `em-stage1-root` sandbox created (aarch64, 1.8G unpacked). All four
  sandboxes created: `em-stage1-root`, `em-stage1-prefix`, `em-stage1-local`,
  `em-stage1-cross` — each `crossdev-stages sandbox setup --arch aarch64
  --name <name>` (few seconds each). Bind-mounted this workspace's own
  `portage-repo/gentoo` tree (already has a real md5-cache) at
  `<sandbox>/var/db/repos/gentoo`, plus `/proc`, `/dev` (rbind), `/sys`,
  `/etc/resolv.conf` (copied — chroot doesn't get one by default, breaks
  distfile fetch DNS), and the host's `/var/cache/distfiles` bind-mounted at
  the same path (cache hits avoid needing network for most distfiles).
- `em --help` and a `toolchain --setup -p` resolve both work cleanly inside
  the bare chroot (`em-stage1-root`, scenario 1): 53 packages, no
  REQUIRED_USE violation, no autounmask needed.
- **Scenario 1 (native `--root`) — full success.** `em toolchain --setup
  --root /root/stage1-root --jobs 1 --keep-going` completed: "35 package(s)
  merged into /root/stage1-root", ">>> native toolchain ready", 0 real
  failures (grep for `failed to merge`/`die:`/`>>> Failed` — none). Verified
  the built compiler is genuinely self-contained: `gcc --version` must be
  run via a **nested** `chroot` into `/root/stage1-root` itself (`sudo
  chroot "$SB/root/stage1-root" /usr/bin/...-gcc --version`), not by
  executing a path inside it from the outer sandbox chroot — the target's
  gcc-config symlinks are absolute (`/usr/aarch64-.../gcc-bin/15/...`) and
  only resolve correctly once that offset *is* `/`. Running it the naive
  way (single chroot, full inner path) silently resolves to the sandbox's
  own host gcc instead (a testing-methodology trap, not an `em` bug — worth
  remembering for any future verification of a `--root`-built toolchain).
  Nested-chroot check confirms `gcc-15.2.1_p20260214`, matching the merge
  log exactly.
- **Finding (not a bug): `--prefix`/`--local` toolchain --setup is a weak
  test here.** Per `docs/root-model.md`'s scenario table, `--prefix P` keeps
  `base_root = /` (host) as the "already installed" view and only overlays
  the delta into `P` — `planner installed = host ∪ VDB(P)`. Since these
  sandboxes are real stage3s (already have a working baselayout/binutils/
  glibc/gcc at their own `/`), `toolchain --setup --prefix <dir>` correctly
  sees the toolchain as already satisfied by the base and only shows
  `[ebuild R]` (reinstall, since the atoms are the explicit targets) with no
  dependency closure — there's no bootstrap to observe, because there's
  nothing missing for the overlay to fill in. This is the planner working
  exactly as documented, not a gap. **Consequence for this matrix**:
  scenarios 2/3 (`--prefix`/`--local`) are more meaningfully tested via
  `em stages --stage1` directly (the full `packages.build` list, `USE="-*
  build"`, which can differ from what the stage3 already has installed)
  rather than `toolchain --setup` first — skip straight to the stage1 step
  for these two scenarios and treat any `toolchain --setup` output as a
  planner-correctness sanity check, not a real bootstrap exercise.

- **Real finding: `em stages --stage1 --prefix <dir> -p --autosolve-use`
  reports a genuine REQUIRED_USE violation for `app-alternatives/awk-4`**
  (`^^ ( gawk busybox mawk nawk )`, all four showing disabled) that
  `--autosolve-use` should have resolved but didn't. Root-caused, not yet
  fixed (out of scope for this validation pass per its own rules):

  - `app-alternatives.eclass`'s `_app-alternatives_set_globals` sets
    `IUSE="+gawk busybox mawk nawk"` (only the *first* alternative gets a
    `+` default — "yep, that's a cheap hack", the eclass's own comment) and
    `REQUIRED_USE="^^ ( gawk busybox mawk nawk )"`. Under normal USE, gawk's
    `+` default alone satisfies `^^`. Under `stages --stage1`'s `USE="-*
    build"`, this week's wildcard-reset fix *correctly* suppresses that `+`
    default too (matching real portage — the whole point of that fix), so
    nothing satisfies the constraint anymore. This part is working as
    intended.
  - The gap: `Adapter::cede_required_use` (`repo.rs:600`) has
    `if self.installed_cpvs.contains(cpv) { return; }` as its *first* check
    — before even looking at whether the current desired USE actually
    satisfies REQUIRED_USE. Under `--prefix`, `installed_cpvs` is
    `target_installed_cpvs` seeded from the **host** VDB (`mod.rs:302-324`,
    matching `docs/root-model.md`'s "planner installed = host ∪ VDB(P)" —
    correct for that purpose). Since the sandbox's stage3 base already has
    `app-alternatives/awk-4` installed, this early return fires and Level-C
    never gets a chance to cede/re-decide its flags for the *new* build
    under `-* build`, even though the constraint demonstrably doesn't hold
    for the desired USE this build is about to use.
  - The function's own doc comment says it "skips ceding entirely when the
    constraint already holds" — that's exactly what the later
    `unsatisfied.is_empty()` check (already present, a few lines down)
    covers correctly. The `installed_cpvs.contains(cpv)` early return looks
    like a separate, overly-broad guard (probably intended to avoid
    re-deciding USE for settled/unrelated already-installed packages) that
    happens to also suppress the one case where re-deciding is exactly what
    stage1's use-override rebuild needs. Suspect fix direction: drop the
    `installed_cpvs` early return and rely on the `unsatisfied.is_empty()`
    check alone (which already re-derives from the *current* `cfg`, so a
    genuinely-settled package still short-circuits correctly) — needs its
    own careful pass (why was the separate check added at all? check
    blame/history) before touching it, not a same-session fix.
  - Scope check still needed: does this affect scenario 1 (`--root`, empty
    target, nothing "already installed")? Should not — `installed_cpvs`
    there is empty for a fresh bootstrap. Likely `--prefix`/`--local`-
    specific (and possibly a real, un-tested `--root --update` /
    `--newuse` reinstall-over-existing-VDB case too — worth a follow-up
    check once this matrix is done).

- **Second real finding, scenario 3 (`--local`): `stages --stage1` exits 1
  on a genuine preflight BDEPEND failure**, distinct from the `--prefix`
  finding above (and confirms it by contrast — see below). Log:
  `error: pre-flight dependency check failed`, listing `app-portage/elt-patches`
  needing `app-arch/xz-utils`, `app-arch/zstd` needing `>=dev-build/meson-1.2.3`,
  `sys-libs/glibc` needing `>=sys-devel/gcc-6.2` — all **genuinely present**
  in the sandbox (`which gcc meson xz` and `qlist -Iv` inside the chroot
  confirm `sys-devel/gcc-15.3.0`, `dev-build/meson-1.11.1`,
  `app-arch/xz-utils-5.8.3` are installed and on `PATH`).

  Root cause: `Cli::base_roots()`'s `--local` branch (`cli.rs:567-581`) sets
  `broot: Some(prefix.clone())` — **`--local`'s BROOT is the prefix itself,
  not the host**, by design (the branch's own comment: "standalone
  Gentoo-Prefix, own BROOT... during bootstrap the host compiler is reached
  via PATH, never via a symlink masquerading as a prefix-owned file"). That
  comment describes the *build-execution*-time behavior (a spawned
  build correctly finds `gcc`/`meson`/`xz` via the chroot's inherited
  `PATH` env, no VDB entry needed) — but `preflight.rs`'s BDEPEND check
  (`portage-cli/src/preflight.rs`, `roots.satisfaction_root(DepClass::Bdepend)`)
  has no equivalent "found via PATH, no VDB tracking needed" concept: it
  only ever checks VDB entries at the BROOT it's given, which for `--local`
  is the prefix's own (still-empty, this being a from-scratch bootstrap)
  VDB. So preflight fails a check that the actual build wouldn't have — a
  real gap between what preflight validates and what `--local`'s own design
  comment says should work.

  **Confirms the `--prefix` finding by contrast**: `--prefix`'s BROOT is
  `/` (`cli.rs:592`, the host) — same chroot, same real gcc/meson/xz — and
  its resolve got past preflight to the actual REQUIRED_USE stage, meaning
  preflight's BDEPEND check *does* work correctly when BROOT is a real VDB
  with these tools registered. The `--local` case is the one that needs
  either (a) a documented, deliberate exception in `preflight.rs` for
  `--local`'s PATH-based BDEPEND model, or (b) BROOT weaving in the host's
  VDB for `--local` too (contradicting its own "own BROOT" design intent —
  would need to reconcile with why `--local` was built to *not* do that
  overlay in the first place, likely relocatability: a `--local` prefix is
  meant to be usable standalone/moved, so depending on host VDB state would
  be a regression). Needs its own design pass, not a quick fix here.

  **Positive confirmation of the `--prefix` diagnosis**: this run's own
  `--autosolve-use` output shows it *correctly* ceding all seven
  `app-alternatives/*` `^^` constraints (awk, bzip2, gzip, lex, ninja, tar,
  yacc — all reported as "configured off" → autosolved to the first listed
  alternative) — because under `--local` nothing is "already installed"
  (fresh prefix), so `cede_required_use`'s `installed_cpvs.contains(cpv)`
  early return never fires here. This is exactly the contrast predicted by
  the `--prefix` root-cause analysis above, now empirically confirmed:
  same wildcard-reset-suppressed defaults, same `^^` constraints, but
  autosolve-use works here and doesn't there, and the only difference is
  whether the cpv is already in `installed_cpvs`.

- **CRITICAL BUG FOUND AND FIXED: `--autosolve-use` stack overflow on a
  populated root.** `em stages --stage1 --root /root/stage1-root -p
  --autosolve-use` (scenario 1, native `--root`, real installed toolchain)
  crashed with `fatal runtime error: stack overflow, aborting`. Without
  `--autosolve-use` the same command resolved cleanly (exit 0, reporting the
  7 expected `^^` REQUIRED_USE violations) — isolating the bug precisely to
  Level-C autosolve combined with a real, non-empty installed base.

  Minimal repro found by bisecting the atom list down from the full
  packages.build set: `USE="-* build" em --root <dir> -p --autosolve-use
  app-alternatives/tar` alone crashes when `app-arch/libarchive` (one of
  tar's `^^ ( gnu libarchive )` alternatives) is already installed in
  `<dir>`; the identical command against a fresh/empty root does not crash.
  Reproduced in-process via a new unit test
  (`required_use_exactly_one_with_installed_alternative_does_not_overflow`,
  `portage-atom-pubgrub/src/provider/tests.rs`) — a minimal `^^ ( w x )`
  shape with `w`'s own dependency pre-installed, no chroot/real-repo needed.

  Root cause (found via `perf record --call-graph dwarf` on a `profiling`-
  profile build, then confirmed instantly with `gdb bt` against the debug
  unit test once minimized): `PortageDependencyProvider::branch_installed_ver`/
  `branch_best_installed` (`provider/mod.rs`) — a `choose_version`
  tie-break heuristic ("prefer the alternative reaching a newer installed
  version") — recursed into **any** virtual package reachable from a
  version's merged deps, with **no cycle guard**. Safe for its intended
  target (`Choice`/`SlotChoice` `||`/`:*` provider trees, which are acyclic
  DAGs down to real packages), but `UseDecision` nodes (Level-C `REQUIRED_USE`
  encoding) reference each other **symmetrically** for a `^^` group's
  mutual-exclusion pairs — an inherent 2-cycle this function was never
  designed to enter. It's gated behind `newest_installed_choice_branch`'s
  "at least one candidate branch reaches an installed package" check, which
  is exactly why it only fired once `libarchive` was genuinely installed.

  Fixed (`portage-atom-pubgrub/src/provider/mod.rs`): restricted the
  recursion to `Choice`/`SlotChoice` only (matching the same distinction
  `host_satisfied_on_broot_inner` already makes for a different function),
  plus a `BRANCH_DEPTH_LIMIT = 16` recursion budget threaded through both
  functions as defense-in-depth, since the heuristic's own docs say "one
  level" but the implementation was actually unbounded. Verified: the unit
  test passes, the full workspace suite passes (1210 tests), clippy/fmt
  clean, and the original real-world repro (`em stages --stage1 --root
  /root/stage1-root -p --autosolve-use`) now resolves cleanly — 78
  packages, exit 0, `tar` correctly autosolved to `libarchive`, no crash.

- **Scenario 1 (native `--root`) — real `stages --stage1` build, post-fix.**
  71 of 77 packages merged; 6 failed. Checked each build.log directly
  (not assumed) before categorizing, per the correction on this session's
  earlier premature cross-build triage:
  - `sys-devel/m4-1.4.20`, `sys-apps/coreutils-9.9-r1`, `app-editors/nano-8.7`:
    all three fail with the identical gnulib error (`./stdlib.h:807:20:
    error: expected identifier or '(' before '_Generic'` in `bsearch`'s
    declaration) — a real gnulib/glibc C23 qualifier-generic-function
    incompatibility (upstream fix: Gentoo bug 969219, "Port to C23
    qualifier-generic fns like strchr"). **Verified, not assumed**: found
    the actual fix already sitting in this repo tree at
    `sys-apps/coreutils/files/coreutils-9.9-glibc-2.43-c23.patch`, but it is
    **not wired into the ebuild's `PATCHES` array** (`coreutils-9.9-r1.ebuild`
    `src_prepare`, only lists two unrelated patches plus a separately-fetched
    `MY_PATCH` bundle that predates this Nov-2025 cherry-pick). `m4` has no
    such patch file in this tree at all. This is a real gap in the checked-out
    ebuild tree snapshot's patch set (would hit real `emerge` identically,
    since it's a source-level compile error against this glibc version, not
    anything `em`-specific) — not an `em` bug.
  - `sys-apps/portage-3.0.77-r3`: meson configure fails with `<PythonExternalProgram
    'python3'> is not a valid python or it is missing distutils` — the
    well-known Python 3.12+ removal of the stdlib `distutils` module; this
    ebuild's meson setup isn't distutils-free yet (or needs a shim). Also a
    real, independently-known compatibility gap, not `em`-specific.
  - `dev-lang/python-3.13.11` (×2, once per consumer needing it): "one or
    more distfiles could not be fetched" — not investigated further (likely
    a genuine cache-miss + this specific patch-level release not mirrored,
    same class as the distfile-reliability gaps already tracked in
    `todo/distfile-fetch-reliability.md`), but flagged here rather than
    assumed, since this session's own earlier mistake was asserting
    "legitimate" without checking.
  - **None of these are new findings requiring an `em` fix.** The stack
    overflow above is the real, `em`-specific bug from this run.

- **`cede_required_use` bug scope broadened: also hits `--root` in an
  upgrade/resume scenario, not just `--prefix`.** Testing `ACCEPT_KEYWORDS="~arm64"`
  against the already-populated `/root/stage1-root` (134 packages installed
  from the earlier successful real build, including `sys-apps/gawk-5.3.2`
  and `app-alternatives/awk-4`) re-triggered the exact same
  `installed_cpvs.contains(cpv)` early-return bug: `gawk` needed upgrading
  to `5.4.0-r2` under the newly-accepted testing keywords, but
  `app-alternatives/awk-4` being already-installed skipped Level-C
  reconsideration, re-exposing its `^^ ( gawk busybox mawk nawk )` violation
  with `--autosolve-use` unable to fix it (same shape as the `--prefix`
  finding, just reached via "re-run stage1 against a partially-built root"
  instead of "base = host"). **Not a `--prefix`-specific bug** — any
  scenario where the target already has the package installed can hit it.
  Redone on a fresh `/root/stage1-testing` root instead to get an
  uncontaminated read on `~arm64`.

- **Re-verified after the fix**: redeployed the fixed binary to all four
  sandboxes. Scenario 2 (`--prefix`) and scenario 3 (`--local`) still show
  exactly their previously-diagnosed, separate bugs (the `cede_required_use`
  early-return gap; the preflight-vs-PATH-tools gap) unchanged — confirms
  all three findings this session are independent, and the stack-overflow
  fix introduced no regression in either.

- **Scenario 4 (cross riscv64) — full success.** `em --target
  riscv64-unknown-linux-gnu crossdev --setup --jobs 1 --keep-going`
  completed cleanly end-to-end (binutils → os-headers → gcc-stage1 → libc →
  gcc-stage2, 0 failures): `>>> cross toolchain riscv64-unknown-linux-gnu
  ready in //usr/riscv64-unknown-linux-gnu`. Verified the compiler actually
  works (`riscv64-unknown-linux-gnu-gcc --version` runs). Followed by
  `em --target riscv64-unknown-linux-gnu stages --stage1 -p --autosolve-use`:
  103 packages, exit 0, all seven `app-alternatives/*` `^^` constraints
  correctly autosolved (no unsatisfied-REQUIRED_USE report) — **further
  confirms the `--prefix` root-cause diagnosis above**: cross's target
  sysroot VDB is fresh/empty (nothing "already installed" there, same
  reasoning as `--local`), so `cede_required_use`'s early return doesn't
  fire and Level-C works as designed. Real `stages --stage1` build kicked
  off.

- **`docs/root-model.md` is stale w.r.t. `--local`**: it documents `--root`/
  `--prefix` in detail (the scenario table, BDEPEND/RDEPEND satisfaction
  roots) but has no section for `--local` at all — its BROOT model
  (`broot = prefix`, distinct from both `--root` and `--prefix`) isn't
  captured anywhere in that doc, which is why the preflight gap above took
  direct code reading to find rather than being a documented, deliberate
  divergence. **Follow-up**: add a `--local` row/section to
  `docs/root-model.md` once the preflight-vs-PATH reconciliation above is
  actually decided (documenting the gap before deciding its resolution
  would just need re-editing).

## Why now

This week landed a chain of fixes that all bear directly on whether
`em stages --stage1` actually works end-to-end: the `--root` config-parity
fix, the `is_cross_arch` false-positive fix, the Choice/SlotChoice
host-satisfaction gap, the `initial_depend` target-VDB-weave regression, the
`USE=-*` wildcard-reset correctness fix (which should remove the
`curl[quic]`→`ngtcp2[gnutls]` autounmask blocker that previously stalled
`stages --stage1` — see `todo/useconfig-clone-elimination.md`'s "Original
proposal" section and this week's commits `b9d4fbb`/`44f4195`/`03fd4ec`), and
the `ForceMask`/`UseConfig` perf cleanup (`d4091a3`). None of these were
re-verified against a full `stages --stage1` run after landing — only
against `-p`/`-vp` resolves and `em toolchain --setup` in isolation. This is
the "actually try it end-to-end" pass.

Native `--root` stage1 was last confirmed clean on 2026-06-26/2026-07-03
(`todo/stage-build-shakeout.md` findings #20/#37), but on stale CLI syntax
(`--cross` flag, since replaced by `--target`) and before this week's fixes.
`--prefix`/`--local` stage1 has **never been end-to-end tested** — only
individual package builds under those modes.

## Reconciling the two scenario framings

Two ways of slicing "what to test" were raised; they overlap rather than
multiply:

- **By root mode**: `--root`, `--prefix`, `--local` (`docs/root-model.md`).
- **By bootstrap topology**: native, cross, self-contained.

"Self-contained" isn't a fourth thing to test separately — it's simply how
`em toolchain --setup` → `em stages --stage1` already works under `--root`:
BROOT is always the host's (or, here, the sandbox chroot's) own
already-working toolchain, and the target starts genuinely empty. There is
no "seeded from an existing toolchain" variant in the current design to
contrast it with. So the real, distinct, CLI-supported matrix is **four
scenarios**, covering both framings without redundant combinations (cross
is inherently a `--root`/`--target` sysroot in this project's model — it
was never meant to compose with `--prefix`/`--local`):

| # | Scenario | Flags | Toolchain step | Stage1 step |
|---|---|---|---|---|
| 1 | Native, `--root` | `--root <dir>` | `em toolchain --setup --root <dir>` | `em stages --stage1 --root <dir>` |
| 2 | `--prefix` overlay | `--prefix <dir>` | `em toolchain --setup --prefix <dir>` | `em stages --stage1 --prefix <dir>` |
| 3 | `--local` Gentoo Prefix | `--local <dir>` | `em toolchain --setup --local <dir>` | `em stages --stage1 --local <dir>` |
| 4 | Cross (crossdev), riscv64 | `--target riscv64-unknown-linux-gnu` | `em --target <tuple> crossdev --setup` | `em --target <tuple> stages --stage1` |

Scenario 4 reuses the riscv64 tuple from prior sessions
(`todo/stage-build-shakeout.md`) for direct comparison against the
2026-07-05 baseline, rather than introducing a new target variable.

## Substrate: crossdev-stages sandbox, not a `/tmp` scratch dir

Per [[crossdev-stages-sandbox]] / [[chroot-test-em-method]]: drive `em`
**inside** a clean stage3 chroot as real root, rather than pointing a
host-run `em` at `--root </tmp/...>` — the latter has repeatedly hit
relative-symlink and permission issues unrelated to the thing under test,
and this session's own `/tmp/stage1-foo` has accumulated cross-session
state that makes it unsuitable for a from-scratch re-validation anyway.

**Note**: `crossdev-stages` itself drives *real* `emerge`/`{tuple}-emerge`
(confirmed by reading `crossdev-stages/src/portage.rs` — `cross_emerge`
shells out to the tuple's real crossdev-installed emerge wrapper). Its own
`target setup`/`target stage1`/`sandbox crossdev` commands are **not** used
here — only `sandbox setup`/`sandbox prepare` to get a clean rootfs, then
our own `em` binary is copied in and driven directly.

```sh
cd ~/Sources/crossdev-stages
./target/release/crossdev-stages sandbox setup --arch aarch64 --name em-stage1-matrix
./target/release/crossdev-stages sandbox prepare --name em-stage1-matrix   # host build deps; verify it doesn't assume real emerge-only tooling we don't need
```

Sandbox rootfs: `~/.cache/crossdev-stages/sandboxes/em-stage1-matrix/`.
Two existing sandboxes (`aarch64-20260618T101350Z`, `em-item6-9-v2`) predate
this week's fixes and prior test runs may have left state in them — start
fresh rather than reusing either, so a clean run isn't second-guessed by
leftover VDB/config state.

```sh
cargo build --release -p portage-cli   # from portage-cli/
cp target/release/em ~/.cache/crossdev-stages/sandboxes/em-stage1-matrix/usr/local/bin/em
```

Scenarios 1–3 can share **one** sandbox chroot (each uses its own target
subdirectory: e.g. `/root/stage1-root`, `/root/stage1-prefix`, `~/.gentoo`
for `--local`'s default) since they don't interact — running as real root
inside the chroot (`sudo chroot ... /usr/local/bin/em ...`) covers `--root`
directly and is also sufficient privilege for `--prefix`/`--local`
(unprivileged modes still work fine as root; `--privilege` auto-detects).
Scenario 4 (cross) needs its own sandbox or at least its own subtree since
it also writes a crossdev overlay + sysroot config into the chroot's `/`.

## Per-scenario commands

Run each with `--autosolve-use` (Level C REQUIRED_USE auto-satisfaction —
needed for the known util-linux `su?(pam)` case from prior sessions) and
without `--autounmask-write` initially, so a still-needed unmask/USE-write
shows up as a reportable gap rather than silently mutating the sandbox's
config — the `USE=-*` wildcard-reset fix this week should mean the
previously-blocking `curl[quic]`→`ngtcp2[gnutls]` case no longer appears at
all; if it still does, that's the first thing to re-diagnose.

```sh
# Scenario 1 — native --root
sudo chroot ~/.cache/crossdev-stages/sandboxes/em-stage1-matrix \
  /usr/local/bin/em toolchain --setup --root /root/stage1-root
sudo chroot ~/.cache/crossdev-stages/sandboxes/em-stage1-matrix \
  /usr/local/bin/em stages --stage1 --root /root/stage1-root --autosolve-use --keep-going --buildpkg

# Scenario 2 — --prefix overlay
sudo chroot ~/.cache/crossdev-stages/sandboxes/em-stage1-matrix \
  /usr/local/bin/em toolchain --setup --prefix /root/stage1-prefix
sudo chroot ~/.cache/crossdev-stages/sandboxes/em-stage1-matrix \
  /usr/local/bin/em stages --stage1 --prefix /root/stage1-prefix --autosolve-use --keep-going --buildpkg

# Scenario 3 — --local (defaults to ~/.gentoo inside the chroot)
sudo chroot ~/.cache/crossdev-stages/sandboxes/em-stage1-matrix \
  /usr/local/bin/em toolchain --setup --local
sudo chroot ~/.cache/crossdev-stages/sandboxes/em-stage1-matrix \
  /usr/local/bin/em stages --stage1 --local --autosolve-use --keep-going --buildpkg

# Scenario 4 — cross, riscv64 (separate sandbox/subtree)
sudo chroot ~/.cache/crossdev-stages/sandboxes/em-stage1-cross \
  /usr/local/bin/em --target riscv64-unknown-linux-gnu crossdev --setup
sudo chroot ~/.cache/crossdev-stages/sandboxes/em-stage1-cross \
  /usr/local/bin/em --target riscv64-unknown-linux-gnu stages --stage1 --autosolve-use --keep-going --buildpkg
```

Start with a **`-p`/pretend pass of `stages --stage1`** for every scenario
before the real build (fast, catches resolution regressions — REQUIRED_USE
violations, unexpected package counts, phantom packages like the
`virtual/libintl`/`virtual/libiconv` case this week — without burning
build time). Only proceed to the real (non-`-p`) run once the pretend
output looks sane.

## Success criteria

- `-p` pass: resolves without a REQUIRED_USE violation report (or, if one
  appears, it's a *known* one — util-linux's `su?(pam)`, handled by
  `--autosolve-use`) and without needing `--autounmask-write`.
- Real run: `em toolchain --setup` completes with a working compiler
  (`<root>/usr/bin/<chost>-gcc --version` runs); `em stages --stage1`
  reports 0 failures, or failures that are already-known/understood
  (cross-reference against `todo/stage-build-shakeout.md`'s findings before
  treating anything as new).
- Package count in the ballpark of prior runs for the same scenario where
  one exists (native --root: ~53 stage1 packages per `packages.build`,
  historically; cross riscv64: matches the 2026-07-05 baseline) — a wildly
  different count (much larger *or* smaller) is itself a signal, not just
  failures.
- No scenario should need a manual VDB patch to complete (the
  `app-alternatives/gpg` IUSE-for-VDB bug, finding #36, is still an open,
  deferred bug — if it recurs, that's expected and already tracked, not new).

## Known prior blockers to watch for (don't re-diagnose from scratch)

From `todo/stage-build-shakeout.md`:
- **`--jobs N` (N>1) races on a shared workdir** (finding #35): a chown/build
  race that can produce spurious "failed to merge" reports for packages that
  actually installed fine (verify via the VDB directly before treating a
  reported failure as real). Prefer `--jobs 1` for the first clean-baseline
  pass per scenario; only reintroduce parallelism once each scenario has one
  known-good serial run to compare against.
- **`fowners` chown-to-foreign-user under non-root** (open, facet 1) —
  shouldn't apply here since these runs are as real root inside the chroot,
  but worth confirming it doesn't resurface for `--prefix`/`--local` (which
  are normally unprivileged; running them as root inside the chroot changes
  this compared to their normal real-world use — note in results whether
  this masks anything that would matter for a genuinely unprivileged run).
- **`app-alternatives/gpg` VDB IUSE gap** (finding #36) — deferred, not
  fixed; expect it if gpg/gpgme is in the closure.
- **MAKEOPTS parallelism** — `todo/useconfig-clone-elimination.md`'s
  predecessor plan mentions this as a still-open blocker for the full
  cross-toolchain-under-`--prefix` live run; check whether it's actually hit
  here or was scenario-specific to a different combination.

## Order of execution

1. Scenario 1 (native `--root`) — cheapest to validate against a known-good
   prior baseline; if this regresses, stop and fix before touching the
   others (something this week's fixes broke would be the highest-priority
   finding).
2. Scenario 2 (`--prefix`) and Scenario 3 (`--local`) — new coverage, can run
   in either order or in parallel (separate subtrees, same sandbox).
3. Scenario 4 (cross, riscv64) — slowest (fresh cross toolchain build), most
   likely to hit a still-open issue; do last so scenarios 1–3's results
   aren't blocked on it.

## Out of scope for this pass

- Building all the way to a full `@system`/stage3 — this matrix is about
  `stages --stage1` specifically (the `toolchain → stage1` pipeline), not
  the later stages.
- Fixing anything found — this is a validation pass; findings get recorded
  here (or in `todo/stage-build-shakeout.md`, following its existing
  numbered-finding convention) and triaged afterward, not fixed inline
  mid-sweep unless trivial.
