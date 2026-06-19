# Non-emptytree `-p` gap is BDEPEND, not `--deep` traversal

STATUS: built-package BDEPEND fixed 2026-06-18 (commit a56690d). `em -p firefox`
82 -> 128, now includes everything emerge's 125 does (nothing missing). Built
packages always pull BDEPEND (`broot_filtered`); the within-run trim always runs.
`em -pe` still 383; tests pass.

REMAINING (+1 over-pull, em 126 vs emerge 125):
- ~~libxcrypt/libcrypt abi_x86_32~~ FIXED 2026-06-18 (commit 74d1643): global
  `use.mask` was not forcing flags off over a `+flag` IUSE default, so
  compiler-rt-sanitizers's `+abi_x86_32` stayed on (arm64) and its multilib
  `virtual/libcrypt[abi_x86_32(-)?]` dep pulled 32-bit libcrypt. em -p 128 -> 126.
- ~~`dev-build/cmake` (U, revbump)~~ FIXED 2026-06-18 (`installed-revbump-update-on-prune.md`):
  `em` updated an installed build tool where emerge keeps the satisfied installed
  one. Cause was the `installed_missing_from_repo` set making `Favor` fall through
  to the newest repo version (update-on-prune). The field is removed; under
  `Favor` any satisfying installed cpv is kept even when pruned from the tree.
  `em -p firefox` is now 0 diffs vs emerge.
- Offset `@system` `--root <empty>` — **REAL gap, root-caused 2026-06-19** in a
  clean crossdev-stages stage3 ([[crossdev-stages-sandbox]]). With a truly empty
  target root (`em -p --root /eroot --config-root /` vs
  `PORTAGE_CONFIGROOT=/ emerge --root=/eroot -p`, all `[ebuild N]`, no
  contamination): **em 177 vs emerge 180**, missing `net-libs/{nghttp2,nghttp3,
  ngtcp2}`.

  Same package *names*, same curl USE (`http2 http3 quic` on for both). The diff
  is **host-side build-dep copies**: those three are `net-misc/curl`'s DEPEND +
  BDEPEND (build) *and* RDEPEND (runtime), and they are **not installed on the
  host**. Isolated on `curl` alone: emerge lists each twice — `… to /eroot/`
  (RDEPEND, target) **and** `…` (no suffix → `/`, the build host) — while em
  lists each only once (target). em's broot filter assumes the host provides all
  build deps and drops the BDEPEND/DEPEND edge; when the host actually *lacks*
  the lib, em never schedules the host-side build install, so its offset plan
  would fail to build curl (nghttp2 absent at build time).

  Fix is architectural: in a native `--root` offset, an unsatisfied BDEPEND/DEPEND
  must be scheduled as a **host/BROOT** merge, not silently broot-filtered. This
  is the offset merge-root modeling that the `--prefix` overlay work shelved (see
  [[overlay-merged-sysroot]]: prefer overlayfs + single ESYSROOT over env
  injection). Not a quick fix; ties into that redesign. (`em -pe firefox` and the
  native non-offset basket remain at parity — this is offset-only.)

---


Discovered 2026-06-18 while chasing the "em --deep traversal gap" (sandbox
aarch64, 305 pkgs installed, `www-client/firefox`).

## Counts

| invocation                       | total | note                                  |
|----------------------------------|-------|---------------------------------------|
| `emerge -p` (default)            | 125   | == `emerge --with-bdeps=n`            |
| `emerge -p --with-bdeps=n`       | 125   | required build-deps still included    |
| `emerge -uDp`                    | 131   | +6 = `--deep` slot-bumps              |
| `em -p` (default `with_bdeps=n`) | 82    | **drops 43 required build tools**     |
| `em -p --with-bdeps`             | 128   | includes them, but +3 vs emerge's 125 |
| `em -pe` / `emerge -pe`          | 383   | parity (emptytree forces bdeps on)    |

The 43 em is missing under `-p` are all build tooling: cbindgen, cargo-c, cython,
clang/clang-common, vala, docbook-xml-dtd, xmlto, pybind11, scikit-build-core,
pillow, pygments, glib-utils, gdbus-codegen, … — i.e. firefox's (and its deps')
**BDEPEND**.

## The real divergence

emerge's `--with-bdeps=n` only drops the *optional* BDEPEND of packages that are
**installed and being kept** — it still pulls the BDEPEND of any package being
**built** (cbindgen is strictly required to build firefox). em's
`with_bdeps=false` is coarser: it drops BDEPEND wholesale, including the required
build-deps of newly-built packages, so `em -p firefox` (82) is missing the build
toolchain and would not actually build.

So:
- This is **not** the `--deep` traversal gap. `--deep` only adds the 6 slot bumps
  (125 → 131); em's `--deep` slot bump already works (it just needs the deps in
  the graph, which in non-emptytree they aren't because of the BDEPEND drop).
- `em -p --with-bdeps` (128) over-pulls by 3 vs emerge's 125 — a second, smaller
  BDEPEND-set discrepancy to chase once the default is fixed.

## Likely fix direction

Mirror emerge: always include BDEPEND of packages being **built** (`N`/rebuild),
and let `--with-bdeps` govern only the BDEPEND of packages **not** being rebuilt
(installed + kept). This lives in the broot/BDEPEND filtering
(`provider/solve.rs` `broot_filtered` + `bdepend_trim.rs`) and the
`solve_with_bdeps` wiring in `query/depgraph/mod.rs`. Care: the offset/`@system`
BDEPEND parity (182 == `emerge --with-bdeps=n`, see em-root-characterization.md)
was tuned against the current behaviour — re-verify it after.
