# Cross-emerge vs `em` performance (riscv64-unknown-linux-gnu)

Machine: same host as `benchmarks/bench-cross-emerge.sh` defaults.  
Target: `sys-devel/gcc -p`, `ACCEPT_LICENSE=*` (embedded profile `@FREE` not expanded in `em` yet).

## Wall time (hyperfine, 3 runs)

| Stage | `em` mean ± σ | cross-emerge mean ± σ | Speedup |
|-------|---------------|------------------------|---------|
| **3a** (post-solve host BDEPEND, `--root-aware` flag) | 618.6 ms ± 52.7 ms | 1.638 s ± 0.014 s | **2.65×** |
| **3b** (solver `(package, merge_root)` nodes, auto dual-root) | 595.3 ms ± 25.7 ms | 1.627 s ± 0.003 s | **2.73×** |
| **3c** (3b + BROOT `IDEPEND`/`BDEPEND` host satisfaction, offset dual-root) | 633.2 ms ± 23.6 ms | 1.665 s ± 0.006 s | **2.63×** |

Stage 3c matches emerge merge-list parity (18 packages for `sys-devel/gcc`); wall time is within noise of 3a/3b (~2.6× faster than `{target}-emerge` for `-p`).

## Merge-list parity (no `--with-bdeps`)

| Package | emerge | em 3b | em 3c | em-only extras (3b) |
|---------|--------|-------|-------|---------------------|
| `sys-devel/gcc` | 18 | 22 | **18** | `bzip2`, `perl`, `locale-gen` (+alternatives) — **fixed in 3c** |
| `sys-libs/zlib` | 1 | 1 | 1 | — |
| `virtual/libiconv` | 1 | 1 | 1 | — |

## `--with-bdeps` (still open)

| | emerge | em 3a | em 3b/3c |
|---|--------|-------|----------|
| `sys-devel/gcc` plan size | 18 | 54 (post-solve mini-resolve) | 53 (in-solver host instances) |

Host-root lines (`to /`) are not yet aligned with portage: in-solver host nodes still over-pull when `--with-bdeps` is set. Next: tighter host-edge filtering and within-run `host_installed` growth.

## Dual-root scheduling (3c)

- **Solver:** `(CPN, slot, merge_root)` nodes; dep classes routed per PMS table 8.2.
- **Auto-activation:** crossdev (`CHOST ≠ CBUILD`), `config_root ≠ merge_root`, or `merge_root ≠ /` (native stage/offset).
- **BROOT satisfaction:** host `/var/db/pkg` drops satisfied `BDEPEND`/`IDEPEND` edges (native and cross).
- **Still open:** unsatisfied BROOT deps on pure `--root stage1/` without host profile; `--with-bdeps` over-pull; `@FREE` license groups.

## How to reproduce

```bash
cargo build --release
benchmarks/bench-cross-emerge.sh target/release/em
# baselines: benchmarks/results/cross-stage3a-baseline-2026-06-16.txt
#           benchmarks/results/cross-stage3b-2026-06-16.txt
#           benchmarks/results/cross-stage3c-idepend-2026-06-16.txt
```