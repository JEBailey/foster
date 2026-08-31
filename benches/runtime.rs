use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use foster::vm::{CompileOptions, Machine};

const FIBONACCI_SOURCE: &str = include_str!("../benchmarks/fibonacci.fos");

const STRING_SOURCE: &str = r#"
import core.string

func repeat(value: String, count: Int, result: String) -> String [consume result] {
    branch {
        count <= 0 -> result
        _ -> repeat(value, count - 1, result + value)
    }
}

func main() -> Int {
    repeat("Foster λ ", 32, "").reverse().upper().length
}
"#;

const SYMBOL_SOURCE: &str = r#"
func classify(value: Symbol) -> Int {
    branch value {
        :alpha -> 1
        :beta -> 2
        :gamma -> 3
        _ -> 4
    }
}

func accumulate(count: Int, total: Int) -> Int {
    branch {
        count <= 0 -> total
        _ -> accumulate(count - 1, total + classify(:gamma))
    }
}

func main() -> Int { accumulate(512, 0) }
"#;

const BYTES_SOURCE: &str = r#"
import core.bytes

func grow(value: Bytes, count: Int) -> Bytes {
    branch {
        count <= 0 -> value
        _ -> grow(value.concat(value), count - 1)
    }
}

func main() -> Int {
    grow("Foster bytes".utf8, 7).hex().length
}
"#;

const BYTE_BUFFER_SOURCE: &str = r#"
import core.byte
import core.bytes.buffer as byte_buffer

func fill(buffer: ByteBuffer, count: Int) -> ByteBuffer [consume buffer] {
    return buffer if count <= 0
    buffer.push(Byte.unchecked(65))
    fill(move buffer, count - 1)
}

func main() -> Int {
    let buffer = fill(ByteBuffer.with_capacity(512), 512)
    (move buffer).freeze().length
}
"#;

const LIST_SOURCE: &str = r#"
func grow(values: List<Int>, count: Int) -> List<Int> [consume values] {
    return values if count <= 0
    let next = values.append(count)
    grow(move next, count - 1)
}

func main() -> Int { grow([], 512).length }
"#;

fn runtime_benchmarks(criterion: &mut Criterion) {
    benchmark_workload(criterion, "fibonacci_20", FIBONACCI_SOURCE);
    benchmark_workload(criterion, "string", STRING_SOURCE);
    benchmark_workload(criterion, "symbol", SYMBOL_SOURCE);
    benchmark_workload(criterion, "bytes", BYTES_SOURCE);
    benchmark_workload(criterion, "byte_buffer", BYTE_BUFFER_SOURCE);
    benchmark_workload(criterion, "list", LIST_SOURCE);
}

fn benchmark_workload(criterion: &mut Criterion, name: &str, source: &str) {
    let compilation = foster::compile(source).unwrap();
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

    let mut group = criterion.benchmark_group(format!("vm/{name}"));
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
