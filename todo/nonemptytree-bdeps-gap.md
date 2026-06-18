# Non-emptytree `-p` gap is BDEPEND, not `--deep` traversal

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
