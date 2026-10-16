"""Measure generated empty, scalar, and record calls, optionally against saved binaries.

Build with each checkout's compiler into separate --output directories, then run
the new directory with --baseline OLD_OUTPUT. Timings exclude setup and loading.
"""
import argparse
import json
from pathlib import Path
import random
import statistics
import struct
import subprocess


def run(command):
    result = subprocess.run([str(arg) for arg in command], capture_output=True,
                            text=True, timeout=240)
    if result.returncode:
        raise RuntimeError(f'{command}:\n{result.stdout}\n{result.stderr}')
    return result


def build(compiler, output):
    fields = [f'm{index}' for index in range(16)]
    header = '#include <stdint.h>\ntypedef struct Matrix16 { ' + ''.join(f'float {field};' for field in fields) + '} Matrix16;\nint64_t answer(void);\nint64_t add(int64_t a,int64_t b);\ndouble matrix_sum(Matrix16 matrix);\n'
    (output / 'packet.h').write_text(header, encoding='utf-8')
    (output / 'packet.c').write_text('#include "packet.h"\nint64_t answer(void) { return 42; }\nint64_t add(int64_t a,int64_t b) { return a+b; }\ndouble matrix_sum(Matrix16 matrix) { return ' + '+'.join('matrix.' + field for field in fields) + '; }\n', encoding='utf-8')
    manifest = dict(abi=1, headers=[(output / 'packet.h').as_posix()],
                    sources=[(output / 'packet.c').as_posix()],
                    records=[dict(name='Matrix16', c_type='Matrix16', fields=[dict(name=field, type='f32') for field in fields])],
                    operations=[dict(name='answer', symbol='answer', parameters=[], result='i64'),
                                dict(name='add', symbol='add', parameters=['i64', 'i64'], result='i64'),
                                dict(name='matrix_sum', symbol='matrix_sum', parameters=[dict(record='Matrix16')], result='f64')])
    (output / 'packet.json').write_text(json.dumps(manifest, indent=2), encoding='utf-8')
    run([compiler, 'bridge', output / 'packet.json', '--output', output / 'packet.dll'])
    bindings = (output / 'packet.fos').read_text(encoding='utf-8')
    for name, expression, expected in [('empty', 'answer()', 42), ('scalars', 'add(20, 22)', 42), ('matrix', 'matrix_sum(matrix)', 136)]:
        setup = 'let matrix = Matrix16 { ' + ', '.join(f'{field}: {index + 1}.0' for index, field in enumerate(fields)) + ' }' if name == 'matrix' else ''
        source = bindings.replace('import std.ffi.*', 'import std.ffi.*\nimport std.time.ContinuousClock', 1) + f'''
func main() -> Result<Int, CError> {{
    {setup}
    let clock = ContinuousClock.new()
    assert((try {expression}) == {expected}{'.0' if name == 'matrix' else ''})
    let checksum = {'0.0' if name == 'matrix' else '0'}
    let index = 0
    let start = clock.now()
    while index < 10000 {{
        checksum = checksum + (try {expression})
        index = index + 1
    }}
    let elapsed = start.until(clock.now()).total_nanoseconds()
    assert(checksum == {expected * 10000}{'.0' if name == 'matrix' else ''})
    println("METRIC," + elapsed.as_string() + "," + checksum.as_string())
    Result.Ok(0)
}}
'''
        path = output / f'{name}.fos'
        path.write_text(source, encoding='utf-8')
        run([compiler, 'fmt', path])
        binary = path.with_suffix('.exe')
        run([compiler, 'build', path, '--native', '-o', binary])
        data = bytearray(binary.read_bytes())
        optional = struct.unpack_from('<I', data, 0x3c)[0] + 24
        struct.pack_into('<Q', data, optional + 72, 64 * 1024 * 1024)
        binary.write_bytes(data)
        print('built', name, flush=True)


def measure(output, baseline, samples):
    variants = {'current': output}
    if baseline:
        variants['baseline'] = baseline
    rows = []
    rng = random.Random(1710)
    for case in ['empty', 'scalars', 'matrix']:
        for repetition in range(samples + 1):
            order = list(variants)
            rng.shuffle(order)
            for variant in order:
                result = run([variants[variant] / f'{case}.exe'])
                metric = next(line for line in result.stdout.splitlines() if line.startswith('METRIC,'))
                _, elapsed, checksum = metric.split(',')
                assert float(checksum) == (136 if case == 'matrix' else 42) * 10000
                rows.append(dict(case=case, variant=variant, repetition=repetition,
                                 warmup=repetition == 0, ns_per_call=int(elapsed) / 10000))
                (output / 'results.json').write_text(json.dumps(rows, indent=2), encoding='utf-8')
    lines = []
    for case in ['empty', 'scalars', 'matrix']:
        for variant in variants:
            values = [row['ns_per_call'] / 1000 for row in rows if row['case'] == case and row['variant'] == variant and not row['warmup']]
            lines.append(f'{case} {variant}: median {statistics.median(values):.3f} us/call, range {min(values):.3f}..{max(values):.3f}')
    report = '\n'.join(lines) + '\n'
    (output / 'report.txt').write_text(report, encoding='utf-8')
    print(report, flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, default=Path('target/ci-effects-check/debug/foster.exe'))
    parser.add_argument('--output', type=Path, default=Path('target/c-argument-packets'))
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--samples', type=int, default=7)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--build-only', action='store_true')
    mode.add_argument('--run-only', action='store_true')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if not args.run_only:
        build(args.compiler.resolve(), output)
    if not args.build_only:
        measure(output, args.baseline.resolve() if args.baseline else None, args.samples)
