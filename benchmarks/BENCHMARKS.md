# Benchmarks Overview (for blogpost / reproducibility)

This document collects **all** benchmark code, data, and reproduction instructions from the portage-cli workspace.

**Current as of HEAD** (confirm with `git rev-parse HEAD` and `cargo check -p portage-bench --benches`).

All microbenchmarks use [criterion](https://crates.io/crates/criterion). Wall-clock use [hyperfine](https://github.com/sharkdp/hyperfine).

## Machines

Per-machine hardware characterization, NUMA details, freq behavior, available trees, and notes for reproducibility are kept in `benchmarks/machines/`.

See `machines/README.md` for overview.

- [thalia](machines/thalia.md) — Current AmpereOne server (128 cores, 4 NUMA nodes, 256 GiB). Use this for new runs. Includes recommendations for `numactl` binding.
- [mneme](machines/mneme.md) — Apple M2 Max laptop. Historical data from prior runs; **fresh full benchmark runs (micro + sweep + comparisons) to be executed later** on the actual M2 hardware (user: "we'll run the benchmarks on the m2 later"). The machine file is prepped with macOS-specific repro instructions, sysctl characterization, verification notes (md5), etc.

When adding a new machine (or fresh run data), create/update `machines/<name>.md` with characterization output + notes, then link here.

**Always** record and link the relevant machine .md when publishing new benchmark results. Old numbers from M2 Max are not directly comparable to server-class AmpereOne runs due to core count, memory system, NUMA, etc.

## Collected Runs and Data

All historical and partial runs have been consolidated into the per-machine files:
- See `machines/mneme.md` (the M2 Max / "mneme") for full collected historical tables (regen, resolve/solver, parsing, interner micro, search, etc. from MEMORY.md, PROFILES.md, results.md, gentoo-interner/README.md, and prior thalia partials). It now includes a "Collected Historical Runs on mneme" section and detailed prep/instructions for new comparative runs. It also lists **all benchmark bash scripts in the specific crates** (portage-repo/bench*.sh, root bench-regen.sh, benchmarks/ scripts, etc.) for running on mneme.
- See `machines/thalia.md` for current server data and partial fresh numbers.

When new runs are done on mneme, append results + characterization to `machines/mneme.md` and update consolidated views here or in MEMORY.md.

Note: many of the bash scripts for regen and comparisons live in the specific crates (e.g. portage-repo/ has dedicated bench-regen.sh, bench.sh, bench-pk.sh for lib-level and pk comparisons; root has bench-regen.sh for em). Use the per-crate ones when benchmarking components on mneme, in addition to the higher-level ones in benchmarks/. See machines/mneme.md for the full list and usage.

## Fresh Numbers from thalia (AmpereOne) - Partial Run

See `machines/thalia.md` for full specs.

While waiting for clean state (no other builds/numactl users), a reduced-sample run of dep_parsing bench was completed (using default features, all-NUMA bind).

**IUse::parse_line microbenchmarks (this machine):**

```
comparison/IUse::parse_line/pkgcraft/small
                        time:   [509.72 ns 509.79 ns 509.83 ns]

comparison/IUse::parse_line/portage-metadata/large
                        time:   [6.3247 µs 6.3266 µs 6.3285 µs]

comparison/IUse::parse_line/pkgcraft/large
                        time:   [8.4182 µs 8.4191 µs 8.4214 µs]
```

(Notes from run: "performance has improved" messages from criterion's change detection vs. previous baseline on this machine. Full interner/resolve/dedup runs were heavy to compile; re-run with `numactl -N0 -m0` once other agents finish.)

These give single-thread parse perf on the 128-core AmpereOne. Add more results to this doc (or a results/ subdir) as they complete, always referencing the machine file.

## Fresh Run on thalia (AmpereOne, 2026-06-14) - Cache Regen and Dep Resolution

See `machines/thalia.md` (the "Full Blown Scans Re-run" section) for full hardware, exact logs, per-crate script outputs (including RSS), verify file counts, and all repro commands. The data below is a summary of the post-fixes re-run using only full blown scans (fresh empty target dirs on the portage-repo/gentoo test tree for regen; default repo discovery for dep -p).

All comparisons executed via the bash scripts that live in the crates (`portage-repo/bench-*.sh`, top-level `bench-regen.sh`, `benchmarks/scripts/compare-regen.sh`, `benchmarks/bench-em-vs-emerge.sh`) after fixing CLI syntax, SCRIPT_DIR setup, and PK discovery paths.

### Cache Regen Comparative on thalia (32k ebuilds test tree, j=8, via compare-regen.sh — em / pk + egencache plain correct on live)

| tool | j | run | real      | user      | sys      | Notes |
|------|---|-----|-----------|-----------|----------|-------|
| em   | 8 | 1   | 0m18.120s | 2m11.310s | 0m12.394s | Current em CLI (NUMA0); full cold exhaustive |
| pk   | 8 | 1   | 0m48.263s | 5m25.512s | 1m7.887s  | pkgcraft (NUMA0); full cold exhaustive |

(egencache uses the stock binary + the sudo rm + sudo --update path inside the script when INCLUDE_EGENCACHE=1. The 4m37.251s j=20 figure is the reference slow full cold point.)

The compare script now includes egencache by default (no INCLUDE needed) using the *exact plain stock* command the user specified for correct full results:

    sudo rm -rf /var/db/repos/gentoo/metadata/md5-cache
    sudo egencache -j $jobs --repo gentoo --update

(hardcoded to live gentoo, no extra args/env). This collects the slow accurate full-cold datapoints (4m37s reference). em/pk use your GENTOO_REPO (set to live for same tree). Set INCLUDE_EGENCACHE=0 or SKIP=egencache to disable. No portage source hacked. Earlier "fast" numbers were artifacts. See thalia.md.

Repro (with egencache stock cold points): `GENTOO_REPO=/var/db/repos/gentoo EM=target/release/em PK=../pkgcraft/target/release/pk INCLUDE_EGENCACHE=1 ITERATIONS=1 ./benchmarks/scripts/compare-regen.sh 8 16 20 24 32`

See `machines/thalia.md` for the complete j=8/j=20 tables, per-crate RSS/hyperfine data, dep numbers, and verification that the main test cache was never modified.

### Dependency Resolution Timing (em -p vs emerge -p, hyperfine 5 runs; from bench-em-vs-emerge.sh)

See `machines/thalia.md` ("Dep Resolution Full Scans" subsection) for the verbatim parity table + exact mean ± σ + "RESULT: parity FAILED" note from the fresh run (background task that tee'd /tmp/dep-compare-thalia.log).

Summary:
- firefox: em 1.003 s ± 0.155 s vs emerge 3.875 s ± 0.037 s (**3.86 ± 0.60 x**)
- libreoffice: em 1.180 s ± 0.158 s vs 4.186 s ± 0.027 s (**3.55 ± 0.47 x**)
- multi (5 pkgs): em 1.028 s ± 0.123 s vs 5.015 s ± 0.118 s (**4.88 ± 0.60 x**)
- em -s gcc: 100.3 ms ± 34.9 ms vs 5.199 s (**~51.8 x**)

Repro: `EM=target/release/em ./benchmarks/bench-em-vs-emerge.sh` (or with `RUNS=N`). Exact output from the bg run persisted at benchmarks/results/dep-thalia-5runs-2026-06-14.txt; full block also in thalia.md.

Parity excellent on most; documented small diffs on texlive + multi (emerge backtracking vs em full graph). Full details + the exact output block are in thalia.md.

This data + historical from mneme will be used for blogpost tables. Repro via the scripts in crates (as noted in thalia.md).

## Locations of Benchmarks

### Central harness & scripts
- `benchmarks/` (member `portage-bench`)
  - `benches/*.rs`: dep_parsing, realworld_dep_parsing, resolve, dedup (criterion)
  - `src/main.rs`: custom solver comparison tool (used for profiling)
  - `scripts/`: bench-sweep.sh, bench-eval.sh, compare-*.sh, maint.sh
  - `bench-em-vs-emerge.sh`: parity + timing vs real emerge (for roadmap parity checks)
  - Data: `MEMORY.md`, `PROFILES.md`, `results.md`, `README.md`

### Per-crate microbenchmarks
- `gentoo-interner/benches/interner.rs` + tables in `gentoo-interner/README.md`
- `portage-atom/benches/parsing.rs` (compares to pkgcraft baseline)
- `portage-atom-resolvo/benches/parsing.rs`
- `portage-vdb/benches/vdb.rs`

**No benches** in: portage-cli (binary), portage-repo, portage-metadata, gentoo-core, gentoo-stages, portage-distfiles (they are exercised via the central ones or examples).

See also:
- `docs/architecture.md` (mentions portage-bench)
- `docs/build-roadmap.md` (references bench-em-vs-emerge.sh for parity milestones)

## How to Run / Reproduce (current workspace)

From the portage-cli root:

```sh
# Microbenchmarks only (fast)
cargo bench -p portage-bench                    # all 4
cargo bench -p portage-bench --bench resolve    # solver on real Gentoo data

# With different interner (see features in benchmarks/Cargo.toml)
cargo bench -p portage-bench --no-default-features --features lasso

# For gentoo-interner specifically
cargo bench -p gentoo-interner --bench interner
cargo bench -p gentoo-interner --bench interner --no-default-features --features symbol-table
```

### Full wall-clock + comparison sweeps (needs Gentoo tree + hyperfine)

```sh
# One-time setup
git clone --depth 1 https://github.com/gentoo/gentoo.git gentoo   # ~200-300 MB
cargo install hyperfine

# Optional: pkgcraft for baselines (pinned rev in Cargo.toml; local override via .cargo/config.toml)
# git clone https://github.com/pkgcraft/pkgcraft ../pkgcraft

# Run sweep (builds per config, runs criterion + regen + search + baselines)
cd benchmarks
./scripts/bench-sweep.sh                    # all 6 configs (interner x alloc)
./scripts/bench-sweep.sh --configs papaya-mimalloc --no-criterion

# Evaluate results into tables
./scripts/bench-eval.sh                     # latest
./scripts/bench-eval.sh -o my-report.md     # to file

# Quick CLI comparisons (assumes release build of em)
../bench-em-vs-emerge.sh target/release/em   # or set EM=...
./scripts/compare-regen.sh
./scripts/compare-search.sh
```

**Hardware note for historical results**: Most numbers below are from Apple M2 Max (12 cores), rustc 1.95 / ~1.92 era. Re-run on your machine for blogpost.

**Reproducibility tips**:
- Use shallow clone for consistent ~31k-32k ebuilds.
- Pin jobs for regen: `--regen-jobs 6` or around core count / 2.
- Always verify cache correctness after regen (file count + aggregate md5 of contents).
- For solver benches, the resolve bench loads a real (or filtered) repo and solves real targets.
- Criterion HTML reports go to `target/criterion/`.
- To match old sweeps: use same interner/allocator features + mimalloc.

See `benchmarks/scripts/README.md` and `benchmarks/README.md` for full options.

## Consolidated Tables from Historical Runs

**Sources & dates** (see individual .md for full context/hardware):
- `benchmarks/MEMORY.md` (2025 data, M2 Max)
- `benchmarks/PROFILES.md` (2025-05-16 arm64 profile)
- `benchmarks/results.md` (regen scaling)
- `gentoo-interner/README.md` (micro + resolve)

### 1. Metadata Cache Regen (wall-clock, full shallow Gentoo ~31k ebuilds, 12 threads)

| Config              | Time     | vs pkgcraft |
|---------------------|----------|-------------|
| **papaya-mimalloc** | **10.10s** | 3.2× faster |
| lasso-mimalloc      | 10.17s   | 3.1× faster |
| symbol-table-mimalloc | 10.10s | 3.1× faster |
| symbol-table-default | 12.04s | 2.6× faster |
| lasso-default       | 12.07s   | 2.6× faster |
| papaya-default      | 13.78s   | 2.3× faster |
| **pkgcraft**        | **31.79s** | baseline  |

*Takeaway*: mimalloc ~20% win. Interner secondary. em ~3× faster than pkgcraft.

### 2. Solver Resolve - Load + Provider Build (criterion, full Gentoo)

| Config              | load_repo | build_provider |
|---------------------|-----------|----------------|
| lasso-mimalloc      | **1.531s** | **265.76ms** |
| papaya-mimalloc     | 1.556s   | 273.59ms     |
| symbol-table-mimalloc | 1.582s | 281.76ms   |
| ... (others slower) | ...      | ...          |

### 3. Solver Solve Targets (selected, ms, papaya-mimalloc often wins with mimalloc)

| Target   | papaya-mimalloc (best in many) | Notes |
|----------|--------------------------------|-------|
| firefox  | **6.711ms**                    |       |
| gcc      | **1.301ms**                    |       |
| rust     | **2.641ms**                    |       |
| openssh  | **0.976ms**                    |       |
| python   | **1.415ms**                    |       |

Full matrix in MEMORY.md.

### 4. Atom/Dep Parsing (criterion, vs pkgcraft)

**Simple/medium/complex synthetic**:
| Benchmark | portage-atom (papaya) | pkgcraft | portage-atom faster |
|-----------|-----------------------|----------|---------------------|
| simple    | 262 ns                | 282 ns   | 7%                  |
| medium    | 1.31 µs               | 1.47 µs  | 12%                 |
| complex   | 3.27 µs               | 3.46 µs  | 5%                  |

**Real-world** (large ebuild RDEPENDs):
| Input   | portage-atom (papaya) | pkgcraft | faster by |
|---------|-----------------------|----------|-----------|
| texlive | 38.96 µs              | 68.18 µs | 75%       |
| pandoc  | 17.53 µs              | 28.52 µs | 63%       |
| ffmpeg  | 31.89 µs              | 45.00 µs | 41%       |

### 5. Interner Backend Microbenchmarks (gentoo-interner, 32-core Linux)

| Workload          | Papaya     | Lasso      | symbol_table | Winner       |
|-------------------|------------|------------|--------------|--------------|
| intern_new/100    | 143 µs     | 120 µs     | **62 µs**    | symbol_table |
| intern_new/1000   | 1.18 ms    | 1.41 ms    | **587 µs**   | symbol_table |
| intern_new/10000  | 24.9 ms    | **9.05 ms**| 14.2 ms      | Lasso        |
| ... (see full in gentoo-interner/README.md) | ... | ... | ... | ... |

Real resolve load:
| Bench                | Papaya | Lasso | symbol_table | Winner |
|----------------------|--------|-------|--------------|--------|
| load_repo            | 1.274s | 1.270s| **1.225s**   | symbol_table |
| build_provider       | 632ms  | **583ms** | 622ms | Lasso |

### 6. Regen Parallelism Scaling (hyperfine, example on 12-core)

| -j | Time     | Notes             |
|----|----------|-------------------|
| 4  | ~17.5s   | Underutilized     |
| 6  | **~13.7s** | Optimal        |
| 8  | ~15.5s   | Contention        |
| 12 | ~15.8s   | Higher contention |

(From results.md; always verify correctness with file count + content hash.)

### 7. Solver Profile Comparison (arm64 profile, with USE + keywords)

| Solver  | Packages | Time   |
|---------|----------|--------|
| PubGrub | 316      | 3.0ms  |
| Resolvo | 88       | 1.1ms  |
| Portage | 246      | 2.9s   |

(See PROFILES.md for overlap analysis.)

### 8. Search (wall-clock, negligible interner/alloc effect)

~25-40ms for common queries (gcc, firefox, rust) — I/O bound.

## Re-running for Fresh Blogpost Data

1. Ensure current code: `cargo check -p portage-bench --benches` (and per-crate).
2. Setup shallow repo as above.
3. Run `benchmarks/scripts/bench-sweep.sh --configs papaya-mimalloc` (or full).
4. `./benchmarks/scripts/bench-eval.sh -o blog-tables.md`
5. Augment with `cargo bench` for micro numbers.
6. For parity: `./benchmarks/bench-em-vs-emerge.sh target/release/em`

**To match exact historical**:
- Use same rustc version if possible.
- Note machine (cores, CPU).
- Capture `meta.env` from sweeps.

## Notes for Blogpost

- papaya chosen as default interner for overall profile + zero extra deps.
- mimalloc gives consistent 15-25% wins on hot paths.
- em significantly faster than both pkgcraft and classic portage for these tasks.
- All numbers should be re-run or clearly dated/attributed for the post.

See individual source .md files for full context, caveats, and raw data.

---

*Generated from scattered sources in the repo. Run the scripts on current HEAD to refresh.*