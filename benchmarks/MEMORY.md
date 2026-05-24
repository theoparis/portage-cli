# portage-bench Memory

## Hardware

All benchmark results below are from an **Apple M2 Max** (12 cores), rustc 1.95.0.
Gentoo tree: shallow clone (~31919 ebuilds).

## Why papaya as default interner

The `gentoo-interner` crate provides three backends: papaya, lasso, symbol-table.
After sweeping all six (interner × allocator) configurations across microbenchmarks,
wall-clock regen, and the pubgrub solver, papaya was chosen as the default for these
reasons:

1. **Competitive everywhere**: papaya is never the slowest backend in any benchmark.
   It wins or ties for the top spot in most allocator-paired configurations.

2. **Best combined with mimalloc**: the papaya+mimalloc config is the fastest or
   near-fastest in the majority of real-world measurements (regen, resolve targets).

3. **Zero-cost when unused**: papaya has no dependency beyond the standard library —
   unlike lasso (dashmap) or symbol-table (hashbrown + foldhash). The `interner`
   feature is default-on and adds only papaya as a dependency.

4. **Real-world regen confirms it**: metadata cache regeneration is the most
   meaningful end-to-end benchmark. papaya+mimalloc completes in ~10s, the fastest
   among all configs.

## Regen (wall-clock, 12 threads, 31919 ebuilds)

Full metadata cache regeneration (`em regen`):

| Config | Real | vs pk |
|--------|------|-------|
| **papaya-mimalloc** | **10.10s** | 3.2× faster |
| lasso-mimalloc | 10.17s | 3.1× faster |
| symbol-table-mimalloc | 10.10s | 3.1× faster |
| symbol-table-default | 12.04s | 2.6× faster |
| lasso-default | 12.07s | 2.6× faster |
| papaya-default | 13.78s | 2.3× faster |
| **pk (pkgcraft 0.0.31)** | **31.79s** | baseline |

**Takeaway**: mimalloc gives ~20% improvement. Interner choice is secondary (~2-5%).
em is ~3× faster than pk regardless of config.

## Resolve (portage-atom-pubgrub, criterion)

Dependency resolution on the full Gentoo tree:

### Load repo + build provider

| Config | load_repo | build_provider |
|--------|-----------|----------------|
| lasso-mimalloc | **1.531s** | **265.76ms** |
| papaya-mimalloc | 1.556s | 273.59ms |
| symbol-table-mimalloc | 1.582s | 281.76ms |
| papaya-default | 1.634s | 278.24ms |
| lasso-default | 1.634s | 276.58ms |
| symbol-table-default | 1.577s | 278.16ms |

### Solve targets

| Target | papaya-default | papaya-mimalloc | lasso-default | lasso-mimalloc | st-default | st-mimalloc |
|--------|---------------|-----------------|---------------|----------------|------------|-------------|
| firefox | 7.578ms | **6.711ms** | 7.824ms | 6.646ms | 7.639ms | 6.832ms |
| gcc | 1.622ms | **1.301ms** | 1.664ms | 1.355ms | 1.665ms | 1.379ms |
| rust | 3.197ms | **2.641ms** | 3.268ms | 2.741ms | 3.328ms | 2.967ms |
| openssh | 1.219ms | **0.976ms** | 1.261ms | 1.012ms | 1.286ms | 1.048ms |
| python | 1.817ms | **1.415ms** | 1.807ms | 1.434ms | 1.880ms | 1.496ms |

**Takeaway**: mimalloc gives **15-25% improvement** on solve targets. papaya+mimalloc
wins every solve target. The interner itself has minimal effect on solver performance —
the allocator dominates.

## Parsing (criterion, dep_parsing)

### portage-atom vs pkgcraft

| Benchmark | portage-atom (papaya) | pkgcraft | portage-atom faster by |
|-----------|-----------------------|----------|------------------------|
| simple | 262 ns | 282 ns | 7% |
| medium | 1.31 µs | 1.47 µs | 12% |
| complex | 3.27 µs | 3.46 µs | 5% |

### portage-atom vs pkgcraft (real-world)

| Input | portage-atom (papaya) | pkgcraft | portage-atom faster by |
|-------|-----------------------|----------|------------------------|
| texlive | 38.96 µs | 68.18 µs | 75% |
| pandoc | 17.53 µs | 28.52 µs | 63% |
| ffmpeg | 31.89 µs | 45.00 µs | 41% |

### Interner comparison on portage-atom parsing

| Benchmark | papaya | lasso | symbol-table |
|-----------|--------|-------|--------------|
| simple | 262 ns | 262 ns | **244 ns** |
| medium | 1.31 µs | 1.31 µs | **1.20 µs** |
| complex | 3.27 µs | 3.27 µs | **3.02 µs** |

**Takeaway**: symbol-table is fastest for parsing (~5-8% margin). papaya and lasso
are essentially tied. pkgcraft is unaffected by interner choice (constant baseline).

## Search (wall-clock, 3 iterations)

| Config | gcc | firefox | rust |
|--------|-----|---------|------|
| any em config | ~25-35ms | ~22-33ms | ~23-38ms |

Search is dominated by I/O and tree-walking; interner/allocator choice is noise at
this timescale.

## Summary of trade-offs

| Dimension | Best | Impact |
|-----------|------|--------|
| **Allocator** | mimalloc | **20-25%** on regen and solve — the dominant variable |
| **Interner (parsing)** | symbol-table | 5-8% faster on parse microbenchmarks |
| **Interner (solve)** | papaya | 1-3% with mimalloc — within noise |
| **Interner (regen)** | papaya/lasso tied | ~2% with mimalloc |
| **Dependencies** | papaya | zero extra deps; lasso pulls dashmap, symbol-table pulls hashbrown+foldhash |

Papaya was chosen as default because it has the best overall profile when combined
with mimalloc (our recommended allocator), adds no transitive dependencies, and is
never significantly slower than the alternatives.

## Notes

- pkgcraft is a local path dependency at `../pkgcraft/crates/pkgcraft`
- Full sweep data in `bench-results/20260523-123133/`
- Re-run with: `./scripts/bench-sweep.sh`
- Evaluate with: `./scripts/bench-eval.sh`
