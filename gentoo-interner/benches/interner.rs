//! Microbenchmarks comparing interner backends.
//!
//! Run with `cargo bench --bench interner` (papaya default) or
//! `cargo bench --bench interner --no-default-features --features lasso` (lasso).
//!
//! Workloads:
//!   - intern_new:      first-time intern of N unique strings (slow path)
//!   - intern_existing: re-intern of N strings already in the table (fast path)
//!   - resolve_dense:   resolve N keys in tight loop (hot read path)
//!   - mixed_st:        single-threaded interleaved intern + resolve
//!   - mixed_mt:        multi-threaded mixed workload (4 threads)

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

use gentoo_interner::{DefaultInterner, Interned};

/// Generate a unique string set that won't collide with other benchmarks.
fn make_strings(n: usize, prefix: &str) -> Vec<String> {
    let salt = NEXT_SALT.fetch_add(1, Ordering::Relaxed);
    (0..n)
        .map(|i| format!("bench{salt}_{prefix}_{i:08}"))
        .collect()
}

static NEXT_SALT: AtomicUsize = AtomicUsize::new(0);

fn bench_intern_new(c: &mut Criterion) {
    let mut group = c.benchmark_group("intern_new");
    for &n in &[100usize, 1000, 10000] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || make_strings(n, "new"),
                |strings| {
                    for s in &strings {
                        black_box(Interned::<DefaultInterner>::intern(s));
                    }
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

fn bench_intern_existing(c: &mut Criterion) {
    let mut group = c.benchmark_group("intern_existing");
    for &n in &[100usize, 1000, 10000] {
        // Pre-intern the strings once
        let strings = make_strings(n, "existing");
        for s in &strings {
            Interned::<DefaultInterner>::intern(s);
        }
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                for s in &strings {
                    black_box(Interned::<DefaultInterner>::intern(s));
                }
            })
        });
    }
    group.finish();
}

fn bench_resolve_dense(c: &mut Criterion) {
    let mut group = c.benchmark_group("resolve_dense");
    for &n in &[100usize, 1000, 10000] {
        let strings = make_strings(n, "resolve");
        let keys: Vec<Interned<DefaultInterner>> =
            strings.iter().map(|s| Interned::intern(s)).collect();
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                for k in &keys {
                    black_box(k.as_str());
                }
            })
        });
    }
    group.finish();
}

fn bench_mixed_st(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_st");
    for &n in &[1000usize, 10000] {
        // Pre-intern half the strings so we have a mix of fast/slow paths
        let warmed = make_strings(n / 2, "mixed_warm");
        for s in &warmed {
            Interned::<DefaultInterner>::intern(s);
        }
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let mut all = warmed.clone();
                    all.extend(make_strings(n / 2, "mixed_new"));
                    all
                },
                |strings| {
                    for s in &strings {
                        let k = Interned::<DefaultInterner>::intern(s);
                        black_box(k.as_str());
                    }
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

fn bench_mixed_mt(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_mt");
    group.sample_size(20);
    for &threads in &[2usize, 4, 8] {
        let n_per_thread = 1000;
        // Pre-intern half so it's a realistic warm + cold mix
        let warm: Vec<String> = make_strings(n_per_thread / 2, "mt_warm");
        for s in &warm {
            Interned::<DefaultInterner>::intern(s);
        }
        let warm = Arc::new(warm);
        group.throughput(Throughput::Elements((threads * n_per_thread) as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                b.iter_batched(
                    || {
                        (0..threads)
                            .map(|t| {
                                let mut v = (*warm).clone();
                                v.extend(make_strings(n_per_thread / 2, &format!("mt_new_{t}")));
                                v
                            })
                            .collect::<Vec<_>>()
                    },
                    |per_thread_strings| {
                        let handles: Vec<_> = per_thread_strings
                            .into_iter()
                            .map(|strings| {
                                thread::spawn(move || {
                                    for s in &strings {
                                        let k = Interned::<DefaultInterner>::intern(s);
                                        black_box(k.as_str());
                                    }
                                })
                            })
                            .collect();
                        for h in handles {
                            h.join().unwrap();
                        }
                    },
                    BatchSize::SmallInput,
                )
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_intern_new,
    bench_intern_existing,
    bench_resolve_dense,
    bench_mixed_st,
    bench_mixed_mt
);
criterion_main!(benches);
