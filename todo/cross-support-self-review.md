# Cross-support self-review (2026-06-21)

A critical review of the cross-compilation work on `master`, ahead of (1) making
the `portage-solver::Solver` trait cover our needs, (2) landing `resolvo-only` /
moving the depgraph code into `portage-solver`. Honest assessment: the *behaviour*
is right and tested, but the *structure* is ad-hoc — "cross" is defined six
different ways and the whole model lives in the CLI while driving pubgrub
internals through concrete extension methods the trait does not know about.

## The structural problem (the reason it feels ad-hoc)

1. **Cross bypasses the `Solver` trait, by design.** `solver.rs` says so:
   "cross-compilation host/sysroot sets stay bridge-specific extension methods on
   the concrete type; the trait models the common native path." So
   `set_cross_active`, `set_root_deps_rdeps`, `add_sysroot_installed`,
   `add_host_installed` are pubgrub-only. **resolvo has zero cross support** (3
   matches, all BDEPEND comments). ⇒ the moment `resolvo-only` lands or depgraph
   moves to `portage-solver`, cross breaks unless cross becomes a trait-level
   concern (or the CLI explicitly keeps pubgrub for cross and resolvo rejects it).

2. **"Cross" has ~6 non-equivalent definitions.** This is the #1 smell — nothing
   downstream can trust a single predicate, so each site re-derives:
   - `root_aware::detect` → `active = dual_root || cross_arch || offset_build`
     (`root_aware.rs:45-67`) — ORs three *orthogonal* axes together.
   - `CrossContext::is_cross_arch()` = `chost != cbuild` else `sysroot != "/"`
     (`root_aware.rs:30-34`).
   - depgraph `cross_arch = Arch::from_chost(chost)` when `active` (`mod.rs:196`).
   - depgraph `cross_rdeps = cross_arch != host arch` (`mod.rs:302`).
   - depgraph `host_config_stage && is_cross_arch()` (`mod.rs:551`).
   - build shell `CHOST != CBUILD` (both set) (`shell.rs:1114`).
   - provider `cross_active` + `root_deps_rdeps` (two booleans derived from above).
   Three orthogonal axes are conflated: **(a)** dual config≠install root, **(b)**
   foreign arch, **(c)** install offset (`--root stage1/`). `active` being their
   OR is why the keyword fix and the rdeps gate each had to *re-extract* the arch.

3. **The model lives in the CLI but encodes solver policy in three places.**
   `CrossContext`/`detect` are in `portage-cli/query/depgraph/root_aware.rs`, yet
   "what root does this dep install into" is decided across: the CLI (sets
   `cross_active`/`rdeps`), pubgrub `solve.rs` (per-dep-class routing), and a
   post-solve `host_copies.rs` walk. Root policy has no single owner.

## Concrete ad-hoc smells (ranked, cheap → structural)

- **MergeRoot defaults to `Target` for every non-real node** (`package.rs:122`),
  including the synthetic root. That is why `--root-deps=rdeps` silently nuked the
  user's seed targets until I added `&& !package.is_virtual()` to every cross
  branch (`solve.rs`). A footgun: the root's deps should never be subject to
  dep-class routing. Fix: route by an explicit per-node role, not a defaulted
  enum.
- **Magic `by_class` indices** in the cross dep routing (`solve.rs`
  `cross_target_runtime_deps`): `by_class[0]`/`[1]`/`[3]`/`[4]` are
  DEPEND/RDEPEND/PDEPEND/IDEPEND as bare integers, and the rdeps drop is a
  `(!rdeps).then(...).into_iter().flatten()` dance. Wants a named `DepClass`
  accessor.
- **Arch derivation duplicated** — keyword acceptance (`mod.rs:196-202`) and the
  rdeps gate (`mod.rs:302`) independently compute target-vs-host arch from CHOST.
- **Two entry points, one heuristic.** `--cross <tuple>` and manual
  `--config-root/--root` both rely on `detect()` reverse-engineering intent from
  paths. `--cross` is clean sugar; the heuristic detection underneath is the soft
  spot (e.g. a same-arch `--config-root X --root X` is "cross" by `is_cross_arch`
  fallback `sysroot != "/"`).

## What is actually sound (do not throw away)

- `MergeRoot::{Host,Target}` as dual-root node identity is a fine model.
- `--cross` as sugar over `roots()` (`cli.rs`) is clean and composes with
  `--local`/`--prefix`/`--root`.
- The build-shell **toolchain export** (`shell.rs`) is correctly placed (a build
  concern in `portage-repo`), self-contained, and gated sanely (CHOST≠CBUILD +
  `${CHOST}-gcc` on PATH). It is a *sixth* cross definition, but a harmless local
  one.
- The `crossdev/` setup tool (`target.rs`, `mod.rs`) is a cohesive, separate
  concern — not entangled with the solver. No structural debt there.
- The keyword-arch and `--root-deps=rdeps` *behaviours* are correct and tested
  (`root_deps_rdeps_drops_target_depend`, the cross `cli::tests`).

## Implications for the ordered plan

1. **Consolidate first (prereq for everything).** Introduce one value — call it
   `RootModel`/`CrossSpec` (sysroot, target, chost, cbuild, host_arch) with
   *named* derived predicates `is_cross_arch()`, `is_offset()`, `root_deps()` —
   computed once and threaded. Delete the 6 scattered predicates. This is the
   single change that removes most of the ad-hoc feel and is behaviour-preserving.
2. **Decide the trait boundary for cross.** Options, in order of honesty:
   - (a) Keep cross pubgrub-only: the trait gains nothing; the CLI selects pubgrub
     when `is_cross_arch()`, resolvo `resolve_targets` returns `Unsupported` for
     cross targets. Lowest effort, unblocks `resolvo-only` for the native path.
   - (b) Model cross in the trait: installed sets tagged by `MergeRoot`, targets
     tagged by merge root, a `RootDepsPolicy` enum input to `resolve_targets`.
     Both bridges implement or explicitly reject. Correct long-term; resolvo cross
     is then a real (large) task.
3. **Then relocate.** Only after (1)+(2) does moving depgraph → `portage-solver`
   land cleanly: the cross boundary is a typed input, not concrete extension
   methods + CLI heuristics.

Cheap wins worth doing regardless of (2): the `MergeRoot` default/`is_virtual`
guard and the `by_class` magic indices — both remove footguns independent of the
trait decision.
