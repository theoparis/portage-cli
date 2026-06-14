# mneme - Apple M2 Max

**Status**: Historical data from previous runs (2025 era). Fresh benchmark runs on the actual M2 Max hardware are planned for later (see user note: "we'll run the benchmarks on the m2 later").

## Hardware (from previous characterizations)

- **CPU**: Apple M2 Max (ARM64)
  - 12 cores (mix of performance and efficiency cores; typical M2 Max config is 8P + 4E or similar)
  - No traditional SMT; Apple Silicon core design
- **Memory**: Unified memory architecture (typically 32-96 GiB on M2 Max configs; exact amount from historical runs not always recorded, but sufficient for shallow Gentoo clone + builds)
- **OS**: macOS (inferred from paths like `/Users/lu_zero/...` in old scripts/notes)
- **Rust**: rustc 1.95.0 (or contemporary)
- **Gentoo tree**: shallow clone (`git clone --depth 1 https://github.com/gentoo/gentoo.git`) yielding ~31,919 ebuilds

## Key Differences from thalia (AmpereOne Server)

- Laptop-class (12 cores, UMA) vs server (128 cores, 4 NUMA nodes)
- Unified memory (high bandwidth within die) vs discrete NUMA
- macOS vs Linux/Gentoo
- Different power/thermal/freq scaling
- No `numactl` / NUMA binding (use `taskset` if available via homebrew, or rely on macOS scheduler; for repro, note no pinning equivalent easily)
- Tooling: `md5` (not `md5sum`), `sysctl` instead of `lscpu`/`numactl`, different paths

This is why historical numbers can appear "off" compared to thalia runs – different hardware class entirely. Fresh runs on this exact M2 Max will provide better apples-to-apples for certain comparisons.

## Planned / Future Fresh Runs (when on the M2)

When running benchmarks here later:

1. **Characterization** (run at start of session, record in this file or a timestamped note):
   ```sh
   uname -a
   sysctl -n machdep.cpu.brand_string
   sysctl -n hw.ncpu
   sysctl -n hw.memsize
   sysctl -n hw.physicalcpu
   sysctl -n hw.logicalcpu
   # Freq / power info if available
   pmset -g batt  # or powermetrics (may need sudo)
   # Current load
   uptime
   ```

2. **Setup** (follow `benchmarks/README.md` and `scripts/README.md`):
   ```sh
   # In a portage-cli checkout (or the integrated benchmarks dir)
   git clone --depth 1 https://github.com/gentoo/gentoo.git gentoo
   cargo install hyperfine
   # Build release
   cargo build --release -p portage-cli   # or equivalent
   ```

3. **Run microbenchmarks** (no external tree needed):
   ```sh
   cargo bench -p gentoo-interner --bench interner
   cargo bench -p portage-bench --bench dep_parsing
   cargo bench -p portage-bench --bench resolve   # if a gentoo tree is present
   # With features for interner/allocator variants
   cargo bench -p portage-bench --no-default-features --features lasso
   ```

4. **Run wall-clock / sweep** (for regen, search, baselines):
   ```sh
   cd benchmarks
   ./scripts/bench-sweep.sh --configs ...   # adjust as needed
   ./scripts/bench-eval.sh -o report-m2.md
   ```

5. **CLI comparisons**:
   ```sh
   ./bench-em-vs-emerge.sh target/release/em
   ./scripts/compare-regen.sh
   ./scripts/compare-search.sh
   ```

6. **Verification** (macOS-specific):
   - Use `md5 -q` instead of `md5sum` for cache correctness checks (as in old results.md).
   - Example from historical:
     ```sh
     find "$outdir" -type f -exec md5 -q {} \; | sort | md5
     ```

## Historical Results Attribution

Tables currently in `MEMORY.md`, `PROFILES.md`, `results.md`, `gentoo-interner/README.md` etc. originated from runs on this M2 Max (or similar).

When adding fresh M2 data:
- Update this file with exact `sysctl` output, macOS version, RAM amount, etc.
- Add new tables or link updated reports.
- Note date of run.

## Notes for Blogpost

- Clearly label M2 Max results as "Apple M2 Max (laptop, 12-core ARM, unified memory)".
- Compare to thalia (AmpereOne server) with caveats on scale.
- Repro instructions should call out macOS differences (paths, md5, lack of NUMA tools).

See parent `BENCHMARKS.md` (Machines section) and `machines/README.md` for cross-references.
## Collected Historical Runs on mneme (M2 Max)

Data extracted from MEMORY.md, PROFILES.md, results.md, gentoo-interner/README.md, etc. (historical 2025 runs on M2 Max, ~12 cores).
### Cache Regen (wall-clock on shallow Gentoo ~31k ebuilds)

From MEMORY.md (12 threads):

| Config | Real | vs pk |
|--------|------|-------|
| **papaya-mimalloc** | **10.10s** | 3.2× faster |
| lasso-mimalloc | 10.17s | 3.1× faster |
| symbol-table-mimalloc | 10.10s | 3.1× faster |
| symbol-table-default | 12.04s | 2.6× faster |
| lasso-default | 12.07s | 2.6× faster |
| papaya-default | 13.78s | 2.3× faster |
| **pk (pkgcraft 0.0.31)** | **31.79s** | baseline |

From results.md (hyperfine example on 12-core M2):

| -j | Time |
|----|------|
| 4 | ~17.5s |
| 6 | **~13.7s** |
| 8 | ~15.5s |
| 12 | ~15.8s |

Sample: 13.697 s ± 0.100 s for j=6.

From gentoo-interner/README.md (bench-regen.sh):

| Threads | Papaya | Lasso | symbol_table |
|---------|--------|-------|--------------|
| 1 | 2m15.6s | **2m14.4s** | 2m24.8s |
| 20 | 11.64s | **11.48s** | 11.69s |
| 24 | **10.41s** | 10.46s | 10.73s |
| 32 | 11.79s | **11.49s** | 12.27s |

### Dependency Resolution / Solver

From PROFILES.md (with profile, 2025-05-16):

| Solver | Packages | Time |
|--------|----------|------|
| PubGrub | 316 | 3.0ms |
| Resolvo | 88 | 1.1ms |
| Portage | 246 | 2.9s |

From MEMORY.md (portage-atom-pubgrub criterion, load):

| Config | load_repo | build_provider |
|--------|-----------|----------------|
| lasso-mimalloc | **1.531s** | **265.76ms** |
| papaya-mimalloc | 1.556s | 273.59ms |
... (full in file)

Solve targets (ms):

| Target | papaya-mimalloc (often best) |
|--------|------------------------------|
| firefox | **6.711ms** |
| gcc | **1.301ms** |
 etc.

From gentoo-interner (real workload portage-bench resolve ~70k ebuilds?):

Load:

| Bench | Papaya | Lasso | symbol_table |
|-------|--------|-------|--------------|
| load_repo | 1.274s | 1.270s | **1.225s** |
| build_provider | 632ms | **583ms** | 622ms |

Targets (ms, lasso often wins here):

| Target | Papaya | Lasso | symbol_table |
|--------|--------|-------|--------------|
| firefox | 199ms | **175ms** | 195ms |
 etc.

### Parsing Micro (portage-atom)

From MEMORY.md:

portage-atom (papaya) vs pkgcraft:

| Benchmark | portage-atom | pkgcraft | faster |
|-----------|--------------|----------|--------|
| simple | 262 ns | 282 ns | 7% |
| medium | 1.31 µs | 1.47 µs | 12% |
| complex | 3.27 µs | 3.46 µs | 5% |

Real-world:

| Input | portage-atom | pkgcraft | faster |
|-------|--------------|----------|--------|
| texlive | 38.96 µs | 68.18 µs | 75% |
| pandoc | 17.53 µs | 28.52 µs | 63% |
| ffmpeg | 31.89 µs | 45.00 µs | 41% |

Interner comparison (portage-atom):

| | papaya | lasso | symbol-table |
|---|--------|-------|--------------|
| simple | 262 ns | 262 ns | **244 ns** |
 etc.

### Interner Micro (gentoo-interner)

Full table in the file (various workloads, winners vary: symbol_table often for intern, papaya for resolve_dense).

### Search

~25-38ms , I/O bound.

### Other

See full BENCHMARKS.md for consolidated, and thalia.md for newer partial runs (e.g. IUse parse ~510ns small on Ampere).


## Comparative Runs to Perform on mneme (M2 Max)

### 1. Cache Regen Comparative (em vs pkgcraft vs portage/egencache)

This compares metadata cache regeneration performance:
- `em regen` (our Rust portage-cli)
- `pk repo metadata regen` (pkgcraft)
- `egencache --update` (official Portage, opt-in because it modifies the live cache)

Use the dedicated script (run from repo root or adjust paths):

```sh
# On mneme (M2), in portage-cli checkout
# Prerequisites: 
# - Fresh shallow Gentoo tree: git clone --depth 1 https://github.com/gentoo/gentoo.git gentoo
# - Built em: cargo build --release -p portage-cli
# - pkgcraft built (pk binary): usually in ../pkgcraft or built separately; set PK=...
# - Portage installed for egencache (INCLUDE_EGENCACHE=1 to enable; it requires write to repo)
# - hyperfine for timing if not using the script's internal time

cd benchmarks

# Basic: compare at default jobs (e.g. 12 for M2)
GENTOO_REPO=../gentoo EM=../../target/release/em PK=../../../pkgcraft/target/release/pk ITERATIONS=3 ./scripts/compare-regen.sh 12

# Sweep jobs (common for M2: 4,6,8,12)
GENTOO_REPO=../gentoo EM=../../target/release/em PK=../../../pkgcraft/target/release/pk INCLUDE_EGENCACHE=1 ITERATIONS=5 ./scripts/compare-regen.sh 4 6 8 12

# Skip some tools if needed
SKIP=egencache ./scripts/compare-regen.sh ...

# The script outputs table with tool, j, run, real/user/sys times.
# It handles isolation for em/pk ( -o / -p dirs), and backup/restore for egencache.
```

After run, verify correctness:
```sh
# For em/pk outputs
for d in /tmp/regen-*; do echo "$d: $(find "$d" -type f | wc -l) files"; find "$d" -type f -exec md5 -q {} \; | sort | md5; done
```

See `benchmarks/scripts/compare-regen.sh` and `results.md` for more details and historical examples.

### 2. Dependency Resolution Comparative (emerge -p vs em -p)

Compares pretend/merge plan output for package set parity and (optionally) timing.

Use:

```sh
# From repo root, after building em
EM=target/release/em ./benchmarks/bench-em-vs-emerge.sh

# Or with timing (wrap with hyperfine or time)
# For specific atoms or sets, see the script for SINGLE_TARGETS, MULTI etc.
# It extracts package atoms from output and diffs emerge -p vs em -p .

# For pure timing on pretend:
hyperfine --warmup 1 --runs 5 \
  'emerge -p --quiet www-client/firefox' \
  'target/release/em -p www-client/firefox'
```

The script `bench-em-vs-emerge.sh` does parity checks on versioned package lists from the plans. It handles some known divergences (e.g. multi-target backtracking).

For full depgraph timing or more, use `em query depgraph` or similar, but basic is -p vs emerge -p.

### Running Full Suite on mneme

Follow `benchmarks/README.md` and `scripts/README.md`:

- Setup gentoo shallow clone.
- `cargo install hyperfine`
- Build release em and ensure pk available.
- `./benchmarks/scripts/bench-sweep.sh` for micro + regen + search across configs (interner/alloc).
- Then `./benchmarks/scripts/bench-eval.sh` to generate report.md with tables.
- Separately run the compare-*.sh for the cross-tool comparisons above.

Record full `uname -a`, `sysctl ...`, `sw_vers` (for macOS version), RAM, etc. in this mneme.md.

Update tables here with new data, date the run, and link from BENCHMARKS.md / MEMORY.md.

## Benchmark Scripts in the Specific Crates (for runs on mneme)

In addition to the central `benchmarks/` scripts, the individual crates have their own bash scripts for targeted benchmarking (especially cache regen at library level, and comparisons). These are useful for reproducibility and collecting per-component numbers for the blogpost.

### Root level (for full `em` CLI)
- `bench-regen.sh` (at workspace root):
  - Benchmarks `em regen` at multiple thread counts.
  - Supports features for interner (LASSO, SYMBOL_TABLE), dedup, NO_MIMALLOC.
  - Usage: `./bench-regen.sh [jobs...]`
  - Sets up em binary from target/release/em.
  - Good for end-to-end em regen numbers on mneme.

### portage-repo crate (library-level regen and comparisons)
- `portage-repo/bench-regen.sh`:
  - Benchmarks the `regen_only` example from portage-repo at multiple jobs.
  - Supports DEDUP, LASSO, SYMBOL_TABLE, MIMALLOC, DHAT.
  - Builds with cargo in the crate dir.
  - Usage: `cd portage-repo && ./bench-regen.sh [jobs...]`
- `portage-repo/bench.sh`:
  - Benchmarks portage-repo's regen_only vs pkgcraft's pk metadata regen.
  - Uses hyperfine for accurate timing, tracks peak RSS.
  - Supports PK override, GENTOO_REPO.
  - Usage: `cd portage-repo && ./bench.sh [jobs...]`
- `portage-repo/bench-pk.sh`:
  - Benchmarks only pkgcraft pk metadata regen (for baseline).
  - Usage: `cd portage-repo && ./bench-pk.sh [jobs...]`
- `portage-repo/benchmark.sh`:
  - Older/more comprehensive script: clones gentoo if needed, builds regen_cache and regen_only, runs verification and benchmarks.
  - Good for setup on fresh mneme checkout.
- `portage-repo/FlameGraph/` scripts: for profiling with flamegraphs (record-test.sh, test.sh) – useful for deeper analysis.

### benchmarks/ crate (CLI-level, sweeps, comparisons, search)
- `benchmarks/bench-em-vs-emerge.sh`:
  - For dependency resolution comparative: parity and timing of `emerge -p` vs `em -p` (and search).
  - Extracts package sets, diffs for correctness, optional hyperfine timing.
  - Targets real packages like firefox, etc.
  - Usage: `./benchmarks/bench-em-vs-emerge.sh [path-to-em]`
- `benchmarks/scripts/compare-regen.sh`:
  - The main comparative for cache regen: em regen vs pk vs egencache (Portage).
  - Handles jobs sweeps, iterations, isolation, cache backup for egencache.
  - See full details in previous section.
- `benchmarks/scripts/bench-sweep.sh`:
  - Full sweep across interner/allocator configs: criterion micro + regen + search + baselines (pk, emerge, qsearch).
  - Produces summary.tsv and report.md via bench-eval.sh.
- `benchmarks/scripts/bench-eval.sh`:
  - Evaluates sweep results into nice markdown tables.
- `benchmarks/scripts/compare-search.sh`: comparative for search.
- `benchmarks/scripts/maint.sh`: for cross-crate setup/patching (useful when setting up on mneme).

### Other crates
- gentoo-interner, portage-atom, etc. primarily use criterion in their benches/ dirs (no heavy bash wrappers, but can be invoked via cargo bench with features).
- portage-atom-pubgrub has solver resolution benches.

When running on mneme (M2 Max / macOS):
- These scripts are bash, should work (may need adjustments for macOS paths, nproc -> sysctl hw.ncpu, md5 vs md5sum, no /proc for RSS in some – see macOS notes in this file).
- For crate-specific: `cd <crate> && ./bench-*.sh ...`
- Combine with central for full picture: run portage-repo's for lib regen, root/benchmarks for em, compare-regen for cross-tool.
- Always set GENTOO_REPO to your shallow clone.
- For macOS reproducibility: use `sw_vers`, record exact shell, etc.

These scripts produce the raw data (times, RSS) that feed into the tables in MEMORY.md, results.md, etc.

Use them to collect fresh runs on mneme for the blogpost, confirming against current codebase (all these scripts are part of the tree and should match the committed em, portage-repo, etc.).

**Complete list of benchmark bash scripts in the workspace (as of current codebase):**
- ./bench-regen.sh (root: em regen)
- ./benchmarks/bench-em-vs-emerge.sh (dep res parity/timing emerge-p vs em-p)
- ./benchmarks/scripts/bench-eval.sh (evaluate sweeps)
- ./benchmarks/scripts/bench-sweep.sh (full config sweeps)
- ./benchmarks/scripts/compare-regen.sh (em vs pk vs egencache regen)
- ./benchmarks/scripts/compare-search.sh (search comparisons)
- ./benchmarks/scripts/maint.sh (workspace maint for bench setup)
- ./portage-repo/bench-pk.sh (pk regen)
- ./portage-repo/bench-regen.sh (portage-repo regen_only)
- ./portage-repo/bench.sh (regen_only vs pk)
- ./portage-repo/benchmark.sh (setup + regen benchmarks)

See also `benchmarks/scripts/README.md` and `benchmarks/README.md` for overviews. When on mneme, run `find . -name "*.sh" ... | xargs grep -l bench` or similar to confirm.
