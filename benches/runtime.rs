use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use foster::vm::{CompileOptions, Machine};

const SOURCE: &str = include_str!("../benchmarks/fibonacci.fos");

fn runtime_benchmarks(criterion: &mut Criterion) {
    let compilation = foster::compile(SOURCE).unwrap();
    let unoptimized =
        foster::vm::compile_with_options(&compilation, CompileOptions { optimize: false }).unwrap();
    let optimized =
        foster::vm::compile_with_options(&compilation, CompileOptions { optimize: true }).unwrap();
    foster::vm::verify(&unoptimized).unwrap();
    foster::vm::verify(&optimized).unwrap();

    let unoptimized_machine = Machine::new(&unoptimized);
    let optimized_machine = Machine::new(&optimized);
    assert_eq!(
        unoptimized_machine.run_main().unwrap(),
        optimized_machine.run_main().unwrap()
    );

    let mut group = criterion.benchmark_group("vm/fibonacci_20");
    group
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(5));
    group.bench_function("unoptimized", |bencher| {
        bencher.iter(|| black_box(unoptimized_machine.run_main().unwrap()));
    });
    group.bench_function("optimized", |bencher| {
        bencher.iter(|| black_box(optimized_machine.run_main().unwrap()));
    });
    group.finish();
}

criterion_group!(benches, runtime_benchmarks);
criterion_main!(benches);
