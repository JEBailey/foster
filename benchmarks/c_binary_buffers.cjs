const fs = require('node:fs');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const {performance} = require('node:perf_hooks');
const assert = require('node:assert/strict');
const {createHash} = require('node:crypto');
const repo = path.resolve(__dirname, '..');
const beforeSource = path.resolve(process.argv[2]);
const afterSource = path.resolve(process.argv[3]);
const destination = path.resolve(process.argv[4]);
const repeats = Number(process.argv[5] || 5);
assert.ok(Number.isInteger(repeats) && repeats > 0);
const directory = path.join(repo, 'target/binary-buffer-performance');
fs.mkdirSync(directory, {recursive:true});
// Keep compiler identities stable even if another build replaces the supplied executable.
const before = path.join(directory, 'compiler-before.exe');
const after = path.join(directory, 'compiler-after.exe');
for (const [source, snapshot] of [[beforeSource,before],[afterSource,after]]) {
  if(source !== snapshot) fs.copyFileSync(source,snapshot);
}
function invoke(exe, args) {
  const start = performance.now();
  const result = spawnSync(exe, args, {cwd:repo, encoding:'utf8', timeout:120000, maxBuffer:8*1024*1024});
  assert.equal(result.status, 0, result.stderr || String(result.error));
  return {ms:performance.now()-start, stdout:result.stdout.trim()};
}
const dll = path.join(directory, 'fixture.dll').replaceAll('\\', '/');
invoke(after, ['bridge', 'tests/fixtures/c_bridge/bindings.json', '--output', dll]);
const schema = fs.readFileSync(path.join(directory,'fixture.schema'),'utf8').trim();
const imports = `import core.byte
import core.int
import core.bytes.Bytes
import core.bytes.buffer.ByteBuffer
import core.result.Result
import std.ffi.CBridge
import std.ffi.CArguments
import std.ffi.CError
`;
const cases = [
  {name:'scalar', calls:100000, bytesPerCall:16, setup:'', body:`
        let arguments = CArguments.empty()
        arguments.integer(20)
        arguments.integer(22)
        let result = try bridge.call(0, arguments)
        assert((try result.integer()) == 42)`},
  {name:'buffer', calls:10, bytesPerCall:65536, setup:`
    let buffer = ByteBuffer.empty()
    let index = 0
    while index < 65536 {
        buffer.push(Byte.unchecked(index.modulo(256)))
        index = index + 1
    }
    let bytes = (move buffer).freeze()`, body:`
        let arguments = CArguments.empty()
        arguments.bytes(bytes)
        let result = try bridge.call(5, arguments)
        assert(result.bytes() == bytes)`},
];
const report = {date:new Date().toISOString(), platform:process.platform, arch:process.arch, repeats,
  before:{path:before,sha256:createHash('sha256').update(fs.readFileSync(before)).digest('hex')},
  after:{path:after,sha256:createHash('sha256').update(fs.readFileSync(after)).digest('hex')},
  note:'Optimized native executables; identical sources and C DLL; each sample includes process startup, packet construction, calls, validation, and cleanup. Build times are single observations with compiler caches already populated, not cold build measurements.', cases:[]};
const median = values => [...values].sort((a,b)=>a-b)[Math.floor(values.length/2)];
for (const test of cases) {
  const source = path.join(directory, test.name+'.fos');
  fs.writeFileSync(source, imports+`
func main() -> Result<Int, CError> {
    let bridge = CBridge.at("${dll}", "${schema}")${test.setup}
    let iteration = 0
    while iteration < ${test.calls} {${test.body}
        iteration = iteration + 1
    }
    Result.Ok(42)
}
`);
  const row = {name:test.name,calls:test.calls,bytesPerCall:test.bytesPerCall};
  for (const [label, compiler] of [['before',before],['after',after]]) {
    const executable = path.join(directory,`${test.name}-${label}.exe`);
    const build = invoke(compiler,['build',source,'--native','-o',executable]);
    assert.equal(invoke(executable,[]).stdout,'Result.Ok(42)');
    row[label] = {buildMs:build.ms,executableSha256:createHash('sha256').update(fs.readFileSync(executable)).digest('hex'),samplesMs:[],medianMs:null};
  }
  // Alternate ordering to reduce bias from changing background load.
  for(let sample=0;sample<repeats;sample++) {
    for(const label of sample%2 ? ['after','before'] : ['before','after']) {
      const result=invoke(path.join(directory,`${test.name}-${label}.exe`),[]);
      assert.equal(result.stdout,'Result.Ok(42)');
      row[label].samplesMs.push(result.ms);
    }
  }
  for(const label of ['before','after']) row[label].medianMs=median(row[label].samplesMs);
  row.speedup=row.before.medianMs/row.after.medianMs;
  report.cases.push(row);
  console.log(`${test.name}: ${row.before.medianMs.toFixed(1)} -> ${row.after.medianMs.toFixed(1)} ms (${row.speedup.toFixed(2)}x)`);
}
fs.mkdirSync(path.dirname(destination),{recursive:true});
fs.writeFileSync(destination,JSON.stringify(report,null,2)+'\n');
console.log(`Saved ${destination}`);
