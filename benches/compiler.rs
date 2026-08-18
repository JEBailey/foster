use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use foster::vm::CompileOptions;

const SOURCE: &str = include_str!("../benchmarks/fibonacci.fos");

fn compiler_benchmarks(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("compiler");
    group
        .sample_size(40)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));

    group.bench_function("front_end_and_checked_hir", |bencher| {
        bencher.iter(|| foster::compile(black_box(SOURCE)).unwrap());
    });

    let compilation = foster::compile(SOURCE).unwrap();
    group.bench_function("bytecode_unoptimized", |bencher| {
        bencher.iter(|| {
            foster::vm::compile_with_options(
                black_box(&compilation),
                CompileOptions { optimize: false },
            )
            .unwrap()
        });
    });
    group.bench_function("bytecode_optimized", |bencher| {
        bencher.iter(|| {
            foster::vm::compile_with_options(
                black_box(&compilation),
                CompileOptions { optimize: true },
            )
            .unwrap()
        });
    });

    group.finish();
}

criterion_group!(benches, compiler_benchmarks);
criterion_main!(benches);
