# Portage Repo Regen Benchmarking Guide

This document explains how to benchmark the `em regen` command for the Gentoo Portage repository metadata regeneration.

---

## Prerequisites

1. **Gentoo Repository**:
   ```bash
   git clone --depth 1 https://github.com/gentoo/gentoo.git
   ```

2. **Rust Toolchain**: Version 1.92 or later recommended.

3. **Hyperfine**: Install for benchmarking:
   ```bash
   cargo install hyperfine
   ```

4. **Built Tools**:
   ```bash
   cargo build --release -p portage-cli  # or from portage-cli root: cargo build --release
   ```

---

## Benchmarking Procedure

### 1. Single Run

```bash
# Benchmark with specific parallelism (e.g., -j 6)
hyperfine --warmup 2 --runs 5 \
  './target/release/em --repo /path/to/gentoo regen -o /tmp/regen-j6 -j 6'
```

### 2. Multiple Parallelism Levels

```bash
# Test -j 4, -j 6, -j 8, -j 12 in one command
hyperfine --warmup 2 --runs 5 \
  './target/release/em --repo /path/to/gentoo regen -o /tmp/regen-j4 -j 4' \
  './target/release/em --repo /path/to/gentoo regen -o /tmp/regen-j6 -j 6' \
  './target/release/em --repo /path/to/gentoo regen -o /tmp/regen-j8 -j 8' \
  './target/release/em --repo /path/to/gentoo regen -o /tmp/regen-j12 -j 12'
```

### 3. Verify Cache Correctness

```bash
# Check file count and aggregate hash for each output
for f in /tmp/regen-j*; do
  echo -n "$f: "
  find "$f" -type f | wc -l
  find "$f" -type f -exec md5 -q {} \; | sort | md5
  echo "---"
done
```

- **File count** should match across runs (e.g., 31929 files)
- **Aggregate hash** must be identical for correct operation

---

## Comparing Branches

To compare two brush versions (e.g., baseline vs. PR #1156):

```bash
# Example (adjust paths and branches to your setup):
# cd /path/to/brush
# git checkout for-portage-repo
# cd /path/to/portage-cli
# cargo build --release
# hyperfine --warmup 2 --runs 5 './target/release/em --repo /path/to/gentoo regen -o /tmp/regen-baseline -j 6'

# ... similarly for the other branch

# Verify both produce identical caches
find /tmp/regen-baseline -type f -exec md5 -q {} \; | sort | md5
find /tmp/regen-1156 -type f -exec md5 -q {} \; | sort | md5
```

---

## Expected Results

| `-j` Value | Time (12-core M2 Max) | Notes                  |
|-----------|----------------------|------------------------|
| 4         | ~17.5s               | Underutilized          |
| 6         | **~13.7s**           | Optimal parallelism   |
| 8         | ~15.5s               | Thread contention     |
| 12        | ~15.8s               | Higher contention     |

### Key Observations
- **Correctness first**: Always verify aggregate hash matches
- **Warmup runs**: Use `--warmup 2` to avoid cold-start skew
- **Multiple samples**: `--runs 5` for stable averages
- **Output isolation**: Use separate `-o` paths to avoid interference
- **Thread scaling**: Test `-j` values around your core count (e.g., 6–12 for 12 cores)

---

## Troubleshooting

1. **Cache mismatch**:
   - Verify no files were modified during benchmark
   - Check for non-deterministic behavior (e.g., timestamps in output)

2. **High variance**:
   - Increase `--runs` to 10 or more
   - Check for background processes consuming resources

3. **Build failures**:
   - Clean rebuild: `cargo clean && cargo build --release`

---

## Sample Output

```
Benchmark 1: ./target/release/em --repo /path/to/gentoo regen -o /tmp/regen-j6 -j 6
  Time (mean ± σ):      13.697 s ±  0.100 s
  Range (min … max):    13.589 s … 13.789 s    5 runs

/tmp/regen-j6:
31929
c8d2ba19d21c6dd45cf7d79a7781ff54
```