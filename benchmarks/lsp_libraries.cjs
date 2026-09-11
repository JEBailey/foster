// Compare real Taker consumers using source and compiled dependencies over LSP stdio.
// Usage: node benchmarks/lsp_libraries.cjs [repeat-count]
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const assert = require('node:assert/strict');
const { spawn, spawnSync } = require('node:child_process');
const { pathToFileURL } = require('node:url');
const { performance } = require('node:perf_hooks');
const repo = path.resolve(__dirname, '..');
const exe = path.join(repo, 'target/release/foster.exe');
const taker = path.resolve(repo, '../taker_foster');
const output = path.join(repo, 'target', 'lsp-library-benchmark-' + Date.now());
const repeats = Number(process.argv[2] || 7);
assert.ok(Number.isInteger(repeats) && repeats > 0);
fs.mkdirSync(output, {recursive:true});
const artifact = path.join(output, 'taker.flib');
const buildStart = performance.now();
const build = spawnSync(exe, ['build', taker, '--library', '-o', artifact], {encoding:'utf8'});
assert.equal(build.status, 0, build.stdout + build.stderr);
const buildMs = performance.now() - buildStart;
fs.writeFileSync(path.join(output, 'library-build.log'), build.stdout + build.stderr);
const cases = [];
for (const example of ['calculator', 'unicode']) {
  const source = fs.readFileSync(path.join(taker, 'examples', example, 'src/main.fos'), 'utf8').trimEnd() + '\n\nfunc benchmark_probe() -> Int { 10 }\n';
  for (const mode of ['source', 'flib']) {
    const root = path.join(output, example + '-' + mode);
    fs.mkdirSync(path.join(root, 'src'), {recursive:true});
    const dependency = path.relative(root, mode === 'source' ? taker : artifact).replaceAll('\\', '/');
    fs.writeFileSync(path.join(root, 'foster.toml'), `[package]\nname = "benchmark-${example}"\n[dependencies]\ntaker = { path = "${dependency}" }\n`);
    const file = path.join(root, 'src/main.fos');
    fs.writeFileSync(file, source);
    cases.push({example, mode, root, file, source});
  }
}
class Client {
  constructor(root, profile) {
    this.pending = new Map(); this.messages = []; this.waiters = []; this.id = 0; this.buffer = Buffer.alloc(0); this.stderr = '';
    const env = {...process.env}; delete env.FOSTER_LSP_PROFILE;
    if (profile) env.FOSTER_LSP_PROFILE = '1';
    this.child = spawn(exe, ['lsp'], {cwd:root, env, stdio:['pipe','pipe','pipe']});
    this.child.stderr.on('data', b => this.stderr += b.toString());
    this.child.stdout.on('data', chunk => {
      this.buffer = Buffer.concat([this.buffer,chunk]);
      while (true) {
        const end = this.buffer.indexOf('\r\n\r\n'); if (end < 0) return;
        const length = Number(/Content-Length:\s*(\d+)/i.exec(this.buffer.subarray(0,end).toString())[1]);
        if (this.buffer.length < end+4+length) return;
        const m = JSON.parse(this.buffer.subarray(end+4,end+4+length)); this.buffer = this.buffer.subarray(end+4+length);
        if (m.id !== undefined && this.pending.has(m.id)) {
          const p = this.pending.get(m.id); this.pending.delete(m.id);
          m.error ? p.reject(new Error(JSON.stringify(m.error))) : p.resolve(m.result);
        } else {
          this.messages.push(m);
          for (const waiter of [...this.waiters]) if (waiter.accept(m)) { this.waiters.splice(this.waiters.indexOf(waiter),1); waiter.resolve(m); }
        }
      }
    });
    this.exited = new Promise(resolve => this.child.once('exit',resolve));
  }
  send(method, params, id) { const b = Buffer.from(JSON.stringify({jsonrpc:'2.0',method,params,...(id===undefined?{}:{id})})); this.child.stdin.write(`Content-Length: ${b.length}\r\n\r\n`); this.child.stdin.write(b); }
  request(method,params) { const id = ++this.id; return new Promise((resolve,reject) => {this.pending.set(id,{resolve,reject});this.send(method,params,id);}); }
  diagnostics(uri,version) { const accept = m => m.method === 'textDocument/publishDiagnostics' && m.params.uri === uri && m.params.version === version; const cached = this.messages.find(accept); return cached ? Promise.resolve(cached) : new Promise(resolve => this.waiters.push({accept,resolve})); }
  async stop() { await this.request('shutdown',null);this.send('exit');this.child.stdin.end();await this.exited; }
}
const median = values => {const s = [...values].sort((a,b)=>a-b);const i=Math.floor(s.length/2);return s.length%2?s[i]:(s[i-1]+s[i])/2;};
async function measure(test, profile=false) {
  const client = new Client(test.root, profile); const uri = pathToFileURL(test.file).href;
  const timer = setTimeout(() => {console.error('Timed out',test.example,test.mode,client.stderr);client.child.kill();process.exit(1);},120000);
  try {
    await client.request('initialize',{processId:process.pid,rootUri:pathToFileURL(test.root).href,capabilities:{}});
    client.send('initialized',{});
    const initialStart = performance.now();
    client.send('textDocument/didOpen',{textDocument:{uri,languageId:'foster',version:1,text:test.source}});
    const initial = await client.diagnostics(uri,1);
    const initialMs = performance.now()-initialStart;
    assert.equal(initial.params.diagnostics.filter(d=>d.severity===1).length,0,JSON.stringify(initial));
    const line = test.source.slice(0,test.source.indexOf('func benchmark_probe')).split('\n').length-1;
    const hover = {textDocument:{uri},position:{line,character:8}};
    const cached = [];
    for (let i=0;i<5;i++) { const start=performance.now();const result=await client.request('textDocument/hover',hover);cached.push(performance.now()-start);assert.ok(JSON.stringify(result).includes('benchmark_probe')); }
    const edits = []; let version=1;
    for (const value of [11,12,13]) {
      const start=performance.now();client.send('textDocument/didChange',{textDocument:{uri,version:++version},contentChanges:[{text:test.source.replace('{ 10 }',`{ ${value} }`)}]});
      const diagnostics=await client.diagnostics(uri,version);edits.push(performance.now()-start);
      assert.equal(diagnostics.params.diagnostics.filter(d=>d.severity===1).length,0,JSON.stringify(diagnostics));
    }
    let start=performance.now();client.send('textDocument/didChange',{textDocument:{uri,version:++version},contentChanges:[{text:test.source.replace('{ 10 }','{ true }')}]});
    const invalid=await client.diagnostics(uri,version);const errorMs=performance.now()-start;
    assert.ok(invalid.params.diagnostics.some(d=>d.severity===1&&d.message.includes('Bool')),JSON.stringify(invalid));
    start=performance.now();client.send('textDocument/didChange',{textDocument:{uri,version:++version},contentChanges:[{text:test.source}]});
    const repaired=await client.diagnostics(uri,version);const repairMs=performance.now()-start;
    assert.equal(repaired.params.diagnostics.filter(d=>d.severity===1).length,0,JSON.stringify(repaired));
    await client.stop();
    if (profile) fs.writeFileSync(path.join(output,`${test.example}-${test.mode}-profile.log`),client.stderr);
    return {initialMs,cachedMs:median(cached),editMs:median(edits),errorMs,repairMs,cachedSamples:cached,editSamples:edits};
  } finally {clearTimeout(timer);if(client.child.exitCode===null)client.child.kill();}
}
(async()=>{
  const results={timestamp:new Date().toISOString(),node:process.version,cpu:os.cpus()[0].model,platform:os.platform(),buildMs,artifactBytes:fs.statSync(artifact).size,repeats,samples:[]};
  const save=()=>fs.writeFileSync(path.join(output,'results.json'),JSON.stringify(results,null,2));
  console.log('Output:',output,'; library build ms:',buildMs.toFixed(1));
  // One complete untimed warmup per variant warms filesystem caches, not LSP process caches.
  for (const test of cases) {await measure(test);console.log('Warmup',test.example,test.mode);}
  for (let repeat=0;repeat<repeats;repeat++) {
    for (const example of ['calculator','unicode']) {
      const pair=cases.filter(c=>c.example===example);if(repeat%2)pair.reverse();
      for (const test of pair) {const metrics=await measure(test);results.samples.push({repeat,example,mode:test.mode,...metrics});save();console.log(JSON.stringify({repeat,example,mode:test.mode,initialMs:metrics.initialMs,editMs:metrics.editMs}));}
    }
  }
  for (const test of cases) {await measure(test,true);console.log('Profile',test.example,test.mode);}
  results.summary=cases.map(test=>{const rows=results.samples.filter(s=>s.example===test.example&&s.mode===test.mode);return {example:test.example,mode:test.mode,...Object.fromEntries(['initialMs','cachedMs','editMs','errorMs','repairMs'].map(key=>[key,{median:median(rows.map(r=>r[key])),min:Math.min(...rows.map(r=>r[key])),max:Math.max(...rows.map(r=>r[key]))}]))};});
  save();console.log(JSON.stringify(results.summary,null,2));
})().catch(e=>{console.error(e);process.exitCode=1;});
