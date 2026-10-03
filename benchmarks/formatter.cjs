// Measures formatting and cancellation through a real LSP connection.
// node benchmarks/formatter.cjs <foster.exe> <source.fos> <results.json>
const fs=require("node:fs"),path=require("node:path"),{spawn}=require("node:child_process"),{pathToFileURL}=require("node:url"),assert=require("node:assert/strict"),{performance}=require("node:perf_hooks");
const exe=path.resolve(process.argv[2]);
const destination=path.resolve(process.argv[4]);
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
    this.exited = new Promise(resolve => this.child.once('exit', code => { for(const p of this.pending.values()) p.reject(new Error("LSP exited: "+code+" "+this.stderr)); resolve(code); }));
  }
  send(method, params, id) { const b = Buffer.from(JSON.stringify({jsonrpc:'2.0',method,params,...(id===undefined?{}:{id})})); this.child.stdin.write(`Content-Length: ${b.length}\r\n\r\n`); this.child.stdin.write(b); }
  request(method,params) { const id = ++this.id; return new Promise((resolve,reject) => {this.pending.set(id,{resolve,reject});this.send(method,params,id);}); }
  diagnostics(uri,version) { const accept = m => m.method === 'textDocument/publishDiagnostics' && m.params.uri === uri && m.params.version === version; const cached = this.messages.find(accept); return cached ? Promise.resolve(cached) : new Promise(resolve => this.waiters.push({accept,resolve})); }
  async stop() { await this.request('shutdown',null);this.send('exit');this.child.stdin.end();await this.exited; }
}

(async()=>{const file=path.resolve(process.argv[3]),root=path.dirname(path.dirname(file)),uri=pathToFileURL(file).href,source=fs.readFileSync(file,"utf8"),c=new Client(root,false);const deadline=setTimeout(()=>{console.error("Formatter benchmark timed out");c.child.kill();process.exitCode=1;},60000);try{await c.request("initialize",{processId:process.pid,rootUri:pathToFileURL(root).href,capabilities:{}});c.send("initialized",{});c.send("textDocument/didOpen",{textDocument:{uri,languageId:"foster",version:1,text:source}});await c.diagnostics(uri,1);let start=performance.now();const edits=await c.request("textDocument/formatting",{textDocument:{uri},options:{tabSize:4,insertSpaces:true}});const formattingMs=performance.now()-start;const pending=c.request("textDocument/formatting",{textDocument:{uri},options:{tabSize:4,insertSpaces:true}}).then(()=>({unexpectedSuccess:true}),e=>({error:String(e)}));const id=c.id;await new Promise(r=>setTimeout(r,50));start=performance.now();c.send("$/cancelRequest",{id});const cancellation=await pending;const cancellationMs=performance.now()-start;assert.match(cancellation.error,/-32800/);const result={file,bytes:Buffer.byteLength(source),formattingMs,edits:edits.length,cancellationMs,cancellation};fs.writeFileSync(destination,JSON.stringify(result,null,2));console.log(result);await c.stop();}finally{clearTimeout(deadline);c.child.kill();}})().catch(e=>{console.error(e);process.exitCode=1;});