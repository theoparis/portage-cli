# `em stages --stage1` scenario matrix — full re-validation across root modes

STATUS: plan only, nothing executed yet.

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
