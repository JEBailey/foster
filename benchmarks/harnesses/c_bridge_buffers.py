"""Compare generated copied-array and borrowed-Buffer C calls on Windows x64.

Run after building this checkout's foster executable. Input construction, DLL
loading, and process startup are excluded from internal elapsed measurements.
"""
import argparse
import json
from pathlib import Path
import random
import statistics
import struct
import subprocess


def run(command, **kwargs):
    result = subprocess.run([str(x) for x in command], capture_output=True, text=True,
                            timeout=180, **kwargs)
    if result.returncode:
        raise RuntimeError(f'{command}:\n{result.stdout}\n{result.stderr}')
    return result


def build(compiler, root, output):
    fixture = root / 'tests/fixtures/c_bridge'
    manifest = json.loads((fixture / 'borrowed.json').read_text(encoding='utf-8'))
    (output / 'pattern.h').write_text('#include <stddef.h>\n#include <stdint.h>\nuint8_t *pattern_bytes(size_t *count);\n', encoding='utf-8')
    (output / 'pattern.c').write_text('#include "pattern.h"\n#include <string.h>\nuint8_t *pattern_bytes(size_t *count) { static uint8_t data[65536]; memset(data,7,sizeof(data)); *count=sizeof(data); return data; }\n', encoding='utf-8')
    manifest['headers'] = [(fixture / 'borrowed.h').as_posix(), (output / 'pattern.h').as_posix()]
    manifest['sources'] = [(fixture / 'borrowed.c').as_posix(), (output / 'pattern.c').as_posix()]
    manifest['operations'].append(dict(name='pattern_bytes', symbol='pattern_bytes', result='bytes', length_type='size', borrowed_result=True))
    (output / 'metrics.json').write_text(json.dumps(manifest, indent=2), encoding='utf-8')
    run([compiler, 'bridge', output / 'metrics.json', '--output', output / 'metrics.dll'])
    bindings = (output / 'metrics.fos').read_text(encoding='utf-8')
    for kind in ['i64', 'u8']:
        for mode in ['copied', 'borrowed']:
            typ = 'List<Int>' if kind == 'i64' else 'Bytes'
            setup = 'make_ints(n)' if kind == 'i64' else '(try pattern_bytes()).slice(0, n)'
            if mode == 'borrowed':
                typ = 'Buffer<Int>' if kind == 'i64' else 'Buffer<Byte>'
                setup = 'buffer::integers(n, 7)' if kind == 'i64' else 'buffer::bytes(n, Byte.unchecked(7))'
            source = bindings.replace('import std.ffi.*', 'import std.ffi.*\nimport core.buffer\nimport core.byte\nimport std.process.Arguments\nimport std.time.ContinuousClock', 1) + f'''
func make_ints(n: Int) -> List<Int> {{
    return [] if n == 0
    let values = [7]
    let index = 1
    while index < n {{
        values.push(7)
        index = index + 1
    }}
    values
}}
func invoke(values: {typ}) -> Result<Int, CError> {{ {mode}_{kind}(values) }}
func main(arguments: Arguments) -> Result<Int, CError> {{
    let n = branch arguments.values[0] {{
        "0" -> 0 "64" -> 64 "4096" -> 4096 "65536" -> 65536 _ -> panic("invalid benchmark size")
    }}
    let repeats = branch arguments.values[1] {{
        "1" -> 1 "10" -> 10 "100" -> 100 "1000" -> 1000 _ -> panic("invalid repeat count")
    }}
    let values = {setup}
    assert((try invoke(values)) == n * 7, "warmup checksum")
    let clock = ContinuousClock.new()
    let checksum = 0
    let index = 0
    let start = clock.now()
    while index < repeats {{
        checksum = checksum + (try invoke(values))
        index = index + 1
    }}
    let elapsed = start.until(clock.now()).total_nanoseconds()
    assert(checksum == n * 7 * repeats, "measured checksum")
    println("METRIC," + elapsed.as_string() + "," + checksum.as_string())
    Result.Ok(0)
}}
'''
            path = output / f'{mode}-{kind}.fos'
            path.write_text(source, encoding='utf-8')
            run([compiler, 'fmt', path])
            run([compiler, 'build', path, '--native', '-o', path.with_suffix('.exe')])
            data = bytearray(path.with_suffix('.exe').read_bytes())
            optional = struct.unpack_from('<I', data, 0x3c)[0] + 24
            struct.pack_into('<Q', data, optional + 72, 64 * 1024 * 1024)
            path.with_suffix('.exe').write_bytes(data)
            print('built', path.name, flush=True)


def measure(output, kind, mode, n, repeats):
    result = run([output / f'{mode}-{kind}.exe', n, repeats])
    line = next(line for line in result.stdout.splitlines() if line.startswith('METRIC,'))
    _, elapsed, checksum = line.split(',')
    assert int(checksum) == n * 7 * repeats
    return int(elapsed)


def benchmark(output, samples, seed):
    cases = [(kind, n) for kind in ['i64', 'u8'] for n in ([0, 64, 4096] if kind == 'i64' else [0, 64, 4096, 65536])]
    repetitions = {}
    for kind, n in cases:
        for mode in ['copied', 'borrowed']:
            for count in [1, 10, 100, 1000]:
                elapsed = measure(output, kind, mode, n, count)
                if elapsed >= 20_000_000:
                    break
            repetitions[kind, mode, n] = count
            print('calibrated', kind, mode, n, count, flush=True)
    results = []
    rng = random.Random(seed)
    suffix = '' if seed == 404 else f'-{seed}'
    for sample in range(samples + 1):
        order = [(kind, mode, n) for kind, n in cases for mode in ['copied', 'borrowed']]
        rng.shuffle(order)
        for kind, mode, n in order:
            count = repetitions[kind, mode, n]
            elapsed = measure(output, kind, mode, n, count)
            results.append(dict(kind=kind, mode=mode, elements=n, repeats=count,
                                sample=sample, warmup=sample == 0, elapsed_ns=elapsed,
                                ns_per_call=elapsed / count))
            (output / f'results{suffix}.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
        print('sample', sample, 'complete', flush=True)
    lines = ['Native generated C bridge: identical C sum function per copied/borrowed pair.',
             'Input setup, DLL loading and process startup excluded; scalar arguments/results still use wire packets.',
             'One warmup discarded; randomized order; matching 64 MiB stacks; all checksums verified.',
             f'{samples} measured samples; repeat counts calibrated per variant and size; results normalized per call.',
             'element,elements,input bytes,copied us/call,borrowed us/call,speed ratio,time reduction %']
    for kind, n in cases:
        medians = {mode: statistics.median(row['ns_per_call'] for row in results if row['kind'] == kind and row['elements'] == n and row['mode'] == mode and not row['warmup']) for mode in ['copied', 'borrowed']}
        old, new = medians['copied'], medians['borrowed']
        lines.append(f'{kind},{n},{n*(8 if kind == "i64" else 1)},{old/1000:.3f},{new/1000:.3f},{old/new:.2f},{100*(old-new)/old:.2f}')
    (output / f'report{suffix}.txt').write_text('\n'.join(lines) + '\n', encoding='utf-8')
    print('\n'.join(lines), flush=True)


if __name__ == '__main__':
    root = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, default=root / 'target/ci-effects-check/debug/foster.exe')
    parser.add_argument('--output', type=Path, default=root / 'target/research/ffi-buffer-benchmark')
    parser.add_argument('--run-only', action='store_true')
    parser.add_argument('--build-only', action='store_true')
    parser.add_argument('--samples', type=int, default=7)
    parser.add_argument('--seed', type=int, default=404)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if not args.run_only:
        build(args.compiler.resolve(), root, output)
    if not args.build_only:
        benchmark(output, args.samples, args.seed)
