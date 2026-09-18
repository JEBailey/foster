// Requests during background checking. Edits are unsaved overlays.
// node benchmarks/lsp_snapshots.cjs <foster.exe> <document.fos> [results.json]
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawn } = require('node:child_process');
const { pathToFileURL } = require('node:url');
const { performance } = require('node:perf_hooks');
const { createHash } = require('node:crypto');
const os = require('node:os');

const exe = path.resolve(process.argv[2]);
const file = path.resolve(process.argv[3]);
const uri = pathToFileURL(file).href;
let root = path.dirname(file);
for (let candidate = root; ; candidate = path.dirname(candidate)) {
  if (fs.existsSync(path.join(candidate, 'foster.toml'))) { root = candidate; break; }
  if (path.dirname(candidate) === candidate) break;
}
const original = fs.readFileSync(file, 'utf8');
const source = original + '\nfunc snapshot_probe(value: Int) -> Int { value }\nfunc snapshot_edit() -> Int { 100 }\n';
const offset = source.lastIndexOf('{ value }') + 3;
const prefix = source.slice(0, offset).split('\n');
const position = { line: prefix.length - 1, character: prefix.at(-1).length };
const params = { textDocument: { uri }, position };
const child = spawn(exe, ['lsp'], { stdio: ['pipe', 'pipe', 'pipe'] });
let buffer = Buffer.alloc(0), nextId = 0, stderr = '';
const pending = new Map(), diagnostics = new Map(), waiters = new Map();
child.stderr.on('data', chunk => stderr += chunk);
child.stdout.on('data', chunk => {
  buffer = Buffer.concat([buffer, chunk]);
  while (true) {
    const end = buffer.indexOf('\r\n\r\n');
    if (end < 0) return;
    const length = Number(/Content-Length:\s*(\d+)/i.exec(buffer.subarray(0, end).toString())[1]);
    if (buffer.length < end + 4 + length) return;
    const message = JSON.parse(buffer.subarray(end + 4, end + 4 + length));
    buffer = buffer.subarray(end + 4 + length);
    if (message.id !== undefined) {
      const request = pending.get(message.id);
      pending.delete(message.id);
      if (message.error) request.reject(new Error(JSON.stringify(message.error)));
      else request.resolve(message.result);
    } else if (message.method === 'textDocument/publishDiagnostics' && message.params.uri === uri) {
      diagnostics.set(message.params.version, message.params);
      waiters.get(message.params.version)?.(message.params);
      waiters.delete(message.params.version);
    }
  }
});
function send(method, params, id) {
  const payload = Buffer.from(JSON.stringify({ jsonrpc: '2.0', method, params, ...(id === undefined ? {} : { id }) }));
  child.stdin.write(`Content-Length: ${payload.length}\r\n\r\n`);
  child.stdin.write(payload);
}
function request(method, params) {
  const id = ++nextId;
  return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); send(method, params, id); });
}
function checked(version) {
  return diagnostics.has(version) ? Promise.resolve(diagnostics.get(version))
    : new Promise(resolve => waiters.set(version, resolve));
}
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const timeout = setTimeout(() => { console.error('Timeout', stderr); child.kill(); process.exitCode = 1; }, 120000);
(async () => {
  try {
    await request('initialize', { processId: process.pid, rootUri: pathToFileURL(root).href, capabilities: {} });
    send('initialized', {});
    send('textDocument/didOpen', { textDocument: { uri, languageId: 'foster', version: 1, text: source } });
    const initial = await checked(1);
    assert.equal(initial.diagnostics.filter(d => d.severity === 1).length, 0, JSON.stringify(initial));
    const samples = [];
    for (let version = 2; version <= 4; version++) {
      const start = performance.now();
      send('textDocument/didChange', { textDocument: { uri, version }, contentChanges: [{ text: source.replace('{ 100 }', `{ ${100 + version} }`) }] });
      // The server's 150 ms debounce has elapsed; the expensive check should be active.
      await delay(250);
      const queries = [];
      for (let repeat = 0; repeat < 5; repeat++) {
        for (const method of ['hover', 'completion', 'definition']) {
          const duringCheck = !diagnostics.has(version);
          const begin = performance.now();
          const result = await request('textDocument/' + method, params);
          const ms = performance.now() - begin;
          assert.ok(result, `${method} must return semantic data`);
          if (method === 'hover') assert.match(JSON.stringify(result), /value: Int/);
          if (method === 'completion') assert.ok(result.some(item => item.label === 'value'));
          if (method === 'definition') assert.equal(result.range.start.line, position.line);
          queries.push({ method, ms, duringCheck, completedDuringCheck: !diagnostics.has(version) });
        }
      }
      assert.ok(queries.some(query => query.duringCheck), 'use a document expensive enough to overlap checking');
      assert.equal((await checked(version)).diagnostics.filter(d => d.severity === 1).length, 0);
      samples.push({ version, diagnosticsMs: performance.now() - start, queries });
    }
    const results = { executable: exe, executableSha256: createHash('sha256').update(fs.readFileSync(exe)).digest('hex'),
      document: file, cpu: os.cpus()[0].model, timestamp: new Date().toISOString(), samples };
    if (process.argv[4]) fs.writeFileSync(process.argv[4], JSON.stringify(results, null, 2) + '\n');
    console.log(JSON.stringify(results, null, 2));
    await request('shutdown', null);
    send('exit', null);
    child.stdin.end();
  } finally {
    clearTimeout(timeout);
    if (child.exitCode === null) child.kill();
  }
})().catch(error => { console.error(error, stderr); process.exitCode = 1; });
