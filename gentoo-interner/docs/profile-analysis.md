# Solver-workload profile analysis: papaya vs lasso

This document captures findings from profiling the
`portage-atom-pubgrub` solver under both interner backends
(`papaya` default and `lasso` feature). It is intended as a
reference for anyone re-running the benchmarks on different
hardware.

## Setup

- Hardware: ARM64, 32-core, glibc 2.x.
- Workload: `portage-bench`'s `resolve` benchmark, target
  `targets/portage-atom-pubgrub/firefox`.
- Profiles captured with:
  ```
  perf record -F 997 --call-graph dwarf -g -- \
      cargo bench --bench resolve --profile-time 20 \
      'targets/portage-atom-pubgrub/firefox'
  ```
- Single Gentoo tree, single repo snapshot, every dep cached.
- Both backends ran the same number of iterations under
  `--profile-time 20`.

## Observed wall-clock gap

For the firefox solver target:

| backend | mean per-iter |
|---------|---------------|
| lasso   | ~177 ms       |
| papaya  | ~206 ms       |

The 12-18% gap reproduces across runs and across other solver
targets in the suite. It is not noise.

## What the profiles show

Top hotspots are essentially identical between backends.

| function (hot path)          | papaya % | lasso % |
|------------------------------|---------:|--------:|
| libc allocator (combined)    |    39.3% |   34.5% |
| `Vec::clone` (provider init) |     8.9% |    7.2% |
| `provider::new`              |     4.2% |    4.5% |
| `SmallVec::drop`             |     3.0% |    3.7% |
| `DepEntry::drop`             |     2.9% |    1.9% |
| `gentoo_interner::get_or_intern` | 0.6% |   <0.5% |
| `DashMap::_get` (lasso only) |     n/a  |    1.8% |

The interner's *own* code is under 1% of total time in both
runs. The gap is not in the interner's hot path.

## Where the gap actually lives

The gap is in the libc allocator, not in the interner. Converted
to absolute time per iteration:

| component             | papaya  | lasso  | delta |
|-----------------------|--------:|-------:|------:|
| libc allocator        | ~81 ms  | ~61 ms | +20 ms|
| everything else       | ~125 ms | ~116 ms| +9 ms |

The ~20 ms extra time spent in `_int_malloc` /
`_int_free_merge_chunk` / `unlink_chunk` / `memcpy` /
`cfree` accounts for most of the 29 ms wall-clock gap.

Neither backend calls the allocator directly from the hot
solver path. Both backends produce allocator pressure from the
*same* call sites:

- `PortageDependencyProvider::new` cloning per-CPV dependency
  vectors (`Vec::clone` ~9%).
- Aggregating `class_results.iter().flat_map(|r|
  r.requirements.clone()).collect()` in `VersionDeps::new`.
- `SmallVec` and `DepEntry` drops on dependency vectors.

So the question is: why does the same allocation-heavy code
spend ~20 ms longer in the allocator when the *interner*
underneath is papaya rather than lasso?

## Hypothesis

The backends shape the global allocator's state differently
*before* the hot solver code runs:

- During cache load, both backends intern the same set of
  strings. Each allocates differently:
  - lasso: one DashMap shard's hash table + the lasso arena
    (one big bump-style allocation per arena page).
  - papaya: one papaya HashMap (lock-free CAS, multiple internal
    segments) + `boxcar::Vec` (segmented append-only vec, growing
    in geometric chunks) + per-string `Box::leak` for the heap
    string copy.
- Both end up with comparable retained memory but very different
  free-list state and fragmentation pattern in the glibc arena.
- When `provider::new` then issues thousands of small `Vec`
  allocations, glibc's free-list servicing those allocations
  pays the cost of the fragmentation left behind by the
  interner build.

In other words: the interner's *cleanup state* is what the
solver pays for, not the interner's own operations.

This hypothesis predicts:

- Switching to a different allocator (jemalloc, mimalloc) should
  shrink or eliminate the gap, because non-glibc allocators
  have very different free-list strategies.
- Workloads with less `Vec::clone` traffic in `provider::new`
  should narrow the gap, because they make fewer demands on
  whatever state the interner left in the allocator.

The first prediction is **confirmed** (see *Confirmed result*
below). The second is still pending.

## Confirmed result: mimalloc closes the gap

Re-ran the same firefox resolve bench across a 3×2 matrix of
backends × allocators (glibc 2.x default vs. mimalloc 0.1.51).

The first matrix below was measured with the original
`iter_with_setup` bench shape, which rebuilt the provider
before every iteration. Criterion correctly excluded setup
time from the reported mean, but the wall-clock samples behind
those numbers were dominated by provider construction, not the
actual resolve work. We later corrected the bench to build the
provider once and use `iter()`; both matrices are kept here
because they tell different (both useful) stories.

### Old shape — setup folded into the wall-clock pattern

| backend       | glibc    | mimalloc | mimalloc speedup |
|---------------|---------:|---------:|-----------------:|
| papaya        |   189 ms |  81.3 ms |            2.32× |
| lasso         |   199 ms |  78.8 ms |            2.53× |
| symbol-table  |   184 ms |  81.0 ms |            2.27× |

### Resolve routine only — provider built once, `iter()`

| backend       | glibc    | mimalloc | mimalloc speedup |
|---------------|---------:|---------:|-----------------:|
| papaya        |  30.5 ms |  22.3 ms |            1.37× |
| lasso         |  31.8 ms |  23.2 ms |            1.37× |
| symbol-table  |  33.3 ms |  22.9 ms |            1.45× |

The split clarifies which phase each variable affects:

- **Setup vs. resolve are very different workloads.**
  Provider construction is allocation-heavy (cloning the repo,
  building hashmaps, running convert::convert_deps for every
  CPV). Resolve is dominated by pubgrub's solver internals
  (`unit_propagation`, `Incompatibility::relation`,
  `Version::cmp`, version range arithmetic).

- **Allocator gain is mostly on setup.** The 2.3× speedup we
  attributed to mimalloc was ~2× on the setup path; the resolve
  routine itself only gets ~1.4× from the allocator switch.

- **Backend ranking flips between phases.** Lasso looks
  slightly faster when setup is included, because its backend
  produces better cache locality during the heavy
  build_provider walk. Papaya is slightly faster on the resolve
  routine because its lock-free `get()` is cheaper than
  DashMap's shard lock under read-heavy access. Symbol-table
  loses on both phases on glibc but ties on mimalloc.

- **Differences are noise-level once mimalloc is in.** All
  three backends land within 4% on mimalloc in both phases.
  Pick on soundness/API ergonomics, not on these numbers.

What this tells us:

1. **The 12-18% backend gap was a glibc artifact.** On mimalloc
   all three backends land within 4% of each other, with
   noise-level differences. The "lasso wins" effect we
   originally documented was the glibc free-list reacting
   differently to each backend's allocation pattern during
   provider construction, not anything about the interner's
   hot path.

2. **Allocator choice dominates.** The 2.3× setup speedup and
   1.4× routine speedup from mimalloc are larger than any
   inter-backend difference observed.

3. **Backend rankings move between phases.** See the table
   note above — different phases stress different parts of the
   interner. No single "winner" survives the split.

On the regen workload (portage-repo full-tree, j=20), the
allocator effect is smaller but in the same direction:

| backend       | glibc      | mimalloc   | speedup |
|---------------|-----------:|-----------:|--------:|
| papaya        |   11.36 s  |   9.86 s   |  1.15×  |
| lasso         |   11.69 s  |  10.04 s   |  1.16×  |
| symbol-table  |   11.60 s  |  10.05 s   |  1.15×  |

Regen never had a meaningful backend gap to close (~3% spread),
but mimalloc still buys ~15% real time and ~20% user time at
the cost of ~2× peak RSS (230 MB → 430 MB) — a typical mimalloc
tradeoff for arena-style allocation.

### Operational takeaways

- **Default to mimalloc** for solver-driven binaries (`em`,
  any future portage-resolver tool). The combined setup +
  resolve speedup pays for itself; the doubled RSS is a
  non-issue at our absolute numbers (under 500 MB on a
  70k-ebuild tree).
- **Don't pick a backend on perf grounds.** All three tie on
  mimalloc; the small glibc differences favour different
  backends in different phases. Pick on soundness/API
  ergonomics, not microbench numbers.
- **Resolve-routine hot spots are mostly outside our code.**
  After the `iter()` switch, ~25% of routine time is in
  pubgrub's own solver loop (`unit_propagation`,
  `Incompatibility::relation`, version-range arithmetic).
  Our `prioritize` and `choose_version` together account for
  about 5%. Allocation pressure during the routine is ~7%.

## What this *does not* mean

- "lasso is a faster interner for this workload." The
  microbenchmarks in `benches/interner.rs` show papaya
  is faster at single-thread interning, and competitive
  at multi-thread interning. The gap on solver targets
  comes from indirect allocator effects, not from
  papaya being slow at its job.

- "We should ship lasso as the default." Lasso is a more
  mature implementation with broader test coverage, but
  on a clean comparison of interner work it loses or ties.
  The solver gap is interesting evidence about allocator
  pressure, not a verdict on the backend.

## Suggested next experiments

In rough order of effort vs. expected signal:

1. **Cross-hardware data.** The matrix above is from one ARM64
   box. An x86_64 run with the same `LASSO=1` / `SYMBOL_TABLE=1`
   / `MIMALLOC=1` env-var matrix would tell us whether the
   mimalloc collapse is portable.
2. **Clean up `Vec::clone` in `provider::new`.** Now a code-
   smell fix more than a perf fix — ~7 ms on an 80 ms budget.
   `mem::take` on the produced vectors, or restructuring
   `VersionDeps::new` to consume instead of clone, would make
   the code less sloppy regardless.
3. **Profile with `heaptrack` on mimalloc.** Now that we know
   the allocator dominates, allocation counts and sizes per
   call site tell us where the remaining 80 ms goes, and
   whether further reductions are worthwhile.

## How to reproduce

```sh
# In portage-repo (path-based deps already wired):
cd $WORK/portage-repo
LASSO=1 ./bench-regen.sh 1 20 24 32          # lasso build
SYMBOL_TABLE=1 ./bench-regen.sh 1 20 24 32   # symbol-table build
./bench-regen.sh 1 20 24 32                  # default papaya

# In portage-bench (solver benchmarks, glibc default):
cd $WORK/portage-bench
cargo bench --bench resolve --features lasso        -- 'resolve/targets/portage-atom-pubgrub/firefox'
cargo bench --bench resolve --features symbol-table -- 'resolve/targets/portage-atom-pubgrub/firefox'
cargo bench --bench resolve                         -- 'resolve/targets/portage-atom-pubgrub/firefox'

# Same bench under mimalloc — repeat each line with `,mimalloc`
# appended to the feature list. This is the matrix that
# produced the table above.
cargo bench --bench resolve --features mimalloc              -- 'resolve/targets/portage-atom-pubgrub/firefox'
cargo bench --bench resolve --features 'lasso,mimalloc'        -- 'resolve/targets/portage-atom-pubgrub/firefox'
cargo bench --bench resolve --features 'symbol-table,mimalloc' -- 'resolve/targets/portage-atom-pubgrub/firefox'
```

The `[patch.crates-io]` blocks in `portage-bench/Cargo.toml`
and `portage-repo/Cargo.toml` pin all interner-touching crates
to local paths so that a single `gentoo-interner` version is
in the build graph. Without those patches, `gentoo-core`
pulled from crates.io drags in a second `gentoo-interner`
version and benchmarks silently mix backends.
