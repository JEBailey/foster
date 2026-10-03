// Repeatable CLI and LSP measurements for the embedded library.
// node benchmarks/bundled_library.cjs <foster.exe> <results.json> [repeats]
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const assert = require('node:assert/strict');
const {spawn, spawnSync} = require('node:child_process');
const {pathToFileURL} = require('node:url');
const {performance} = require('node:perf_hooks');
const {createHash} = require('node:crypto');
const repo = path.resolve(__dirname, '..');
const exe = path.resolve(process.argv[2]);
const destination = path.resolve(process.argv[3]);
const repeats = Number(process.argv[4] || 7);
assert.ok(Number.isInteger(repeats) && repeats > 0);
const output = path.dirname(destination);
fs.mkdirSync(output, {recursive:true});
const fixtures = ['benchmarks/fibonacci.fos', 'tests/fixtures/programs/hash_collections.fos', 'tests/fixtures/programs/json.fos', 'tests/fixtures/programs/unicode.fos'];
const cases = fixtures.map(relative => {
  const file = path.join(repo,relative);
  return {example:path.basename(file,'.fos'), mode:'bundled', root:path.dirname(file), file,
    source:fs.readFileSync(file,'utf8').trimEnd() + '\n\nfunc benchmark_probe() -> Int { 10 }\n'};
});
function cli(args) {
  const start = performance.now();
  const result = spawnSync(exe,args,{cwd:repo,encoding:'utf8',timeout:120000});
  assert.equal(result.status,0,result.stdout + result.stderr);
  return performance.now()-start;
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
  const results = {timestamp:new Date().toISOString(), cpu:os.cpus()[0].model, platform:os.platform(), node:process.version,
    executable:exe, executableBytes:fs.statSync(exe).size, executableSha256:createHash('sha256').update(fs.readFileSync(exe)).digest('hex'),
    repeats, sources:fixtures.map(file=>({file,sha256:createHash('sha256').update(fs.readFileSync(path.join(repo,file))).digest('hex')})), cli:[], lsp:[]};
  const save = () => fs.writeFileSync(destination,JSON.stringify(results,null,2));
  const commands = [{name:'startup', args:['--help']}, ...fixtures.flatMap(file=>[
    {name:'check/'+path.basename(file),args:['check',file]},
    {name:'build/'+path.basename(file),args:['build',file,'-o',path.join(output,'probe.fbc')]}
  ])];
  for (const command of commands) cli(command.args);
  for (let repeat=0;repeat<repeats;repeat++) for(const command of commands) {
    const ms=cli(command.args); results.cli.push({repeat,name:command.name,ms});save();
  }
  for(const test of cases) {await measure(test); console.log('Warmup',test.example);}
  for(let repeat=0;repeat<repeats;repeat++) for(const test of cases) {
    const metrics=await measure(test);results.lsp.push({repeat,example:test.example,...metrics});save();
    console.log(JSON.stringify({repeat,example:test.example,initialMs:metrics.initialMs,editMs:metrics.editMs}));
  }
  for(const test of cases) await measure(test,true);
  const summary = (rows,keys) => Object.fromEntries(keys.map(key=>[key,{median:median(rows.map(r=>r[key])),min:Math.min(...rows.map(r=>r[key])),max:Math.max(...rows.map(r=>r[key]))}]));
  results.summary={cli:commands.map(command=>({name:command.name,...summary(results.cli.filter(r=>r.name===command.name),['ms'])})),
    lsp:cases.map(test=>({example:test.example,...summary(results.lsp.filter(r=>r.example===test.example),['initialMs','cachedMs','editMs','errorMs','repairMs'])}))};
  save();console.log(JSON.stringify(results.summary,null,2));
})().catch(e=>{console.error(e);process.exitCode=1;});
