# Scripts

Utility scripts for benchmarking, comparison, and workspace maintenance.

## Benchmark sweep

| script | what it does |
|--------|-------------|
| `bench-sweep.sh` | Full evaluation: builds `em` per (interner × allocator) config, runs criterion benches, `em regen`, `em search`, and baselines (pkgcraft, emerge, qsearch) |
| `bench-eval.sh` | Parses `summary.tsv` from a sweep run, generates a markdown report with comparison tables |

### Quick start

```sh
# Set up the Gentoo tree (one-time, ~200 MB)
git clone --depth 1 https://github.com/gentoo/gentoo.git gentoo

# Full 6-config sweep (papaya/lasso/symbol-table × default/mimalloc)
./scripts/bench-sweep.sh

# Single config, skip criterion, quick regen only
./scripts/bench-sweep.sh --configs papaya-mimalloc --no-criterion --no-search

# Evaluate latest run
./scripts/bench-eval.sh
```

### Output structure

```
bench-results/<timestamp>/
  meta.env                hardware/software info
  <config>/               one dir per config (e.g. papaya-mimalloc)
    em                    built binary for this config
    *.parsed              criterion parsed results
    regen.time            wall-clock regen timing
    search.time           wall-clock search timing
  baselines/
    pk-regen.time         pkgcraft regen baseline
    emerge-search.time    emerge search baseline
  summary.tsv             all data in one TSV
  report.md               generated evaluation report
```

### Options

`bench-sweep.sh` options:

| Flag | Default | Description |
|------|---------|-------------|
| `--configs` | all 6 combos | Comma-separated config list |
| `--repo` | `./gentoo` | Path to Gentoo repo |
| `--regen-jobs` | min(cores, 24) | Thread count for regen |
| `--search-patterns` | gcc,firefox,rust | Search patterns to time |
| `--criterion-args` | | Extra args for criterion |
| `--no-criterion` | | Skip criterion benches |
| `--no-regen` | | Skip regen wall-clock |
| `--no-search` | | Skip search wall-clock |
| `--no-baselines` | | Skip baseline measurements |
| `-n` / `--dry-run` | | Print commands without executing |

## CLI comparison (standalone)

| script | what it compares | tools |
|--------|------------------|-------|
| `compare-regen.sh` | metadata-cache regeneration on a Gentoo tree | em / pk / egencache* |
| `compare-search.sh` | `em search <pat>` vs `emerge -s <pat>` vs `qsearch <pat>` | em / emerge / qsearch† |

These use a pre-built `em` binary (not a sweep). Useful for quick single-config
comparisons on Gentoo.

(*) egencache is opt-in via `INCLUDE_EGENCACHE=1`
(†) qsearch is auto-detected, silently skipped if absent

## Workspace maintenance

| script | what it does |
|--------|-------------|
| `maint.sh` | `setup` / `update` / `patch` / `unpatch` / `status` across all portage-* and gentoo-* crates |

See `maint.sh --help` for details.
