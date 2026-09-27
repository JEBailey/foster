// Build with: node node_modules/typescript/bin/tsc -p . --outDir ../../target/vscode-test-out
// Run from the repository root: node --test editors/vscode/scripts/test-run-debug.cjs
const assert=require('node:assert/strict');
const fs=require('node:fs');const path=require('node:path');const Module=require('node:module');
const {test,after}=require('node:test');const {EventEmitter}=require('node:events');
const root=path.resolve(__dirname,'../../..');const build=path.join(root,'target/vscode-test-out');
const fixture=fs.mkdtempSync(path.join(root,'target/vscode-run-debug-'));
after(()=>{assert.equal(path.dirname(fs.realpathSync(fixture)),fs.realpathSync(path.join(root,'target')));fs.rmSync(fixture,{recursive:true});});
class VSCodeEvents {
  events=new EventEmitter();
  event=(listener)=>{this.events.on('message',listener);return {dispose:()=>this.events.off('message',listener)}};
  fire(message){this.events.emit('message',message)}
  dispose(){this.events.removeAllListeners()}
}
const original=Module._load;
const vscodeMock={EventEmitter:VSCodeEvents,window:{},workspace:{}};
Module._load=function(name,...args){if(name==='vscode')return vscodeMock;return original.call(this,name,...args)};
const {FosterDebugAdapter}=require(path.join(build,'debug.js'));
const {activeTarget,packageTaskTargets}=require(path.join(build,'run.js'));
const {launchOptions,commandArguments,findPackageRoot,taskPath}=require(path.join(build,'launch.js'));
Module._load=original;
const executable=process.env.FOSTER_SERVER_PATH||path.join(root,'target/debug',process.platform==='win32'?'foster.exe':'foster');
const file=path.join(fixture,'main.fos');fs.writeFileSync(file,'func main() -> Int {\n    let value = 42\n    value\n}\n');

class Client {
  sequence=0;pending=new Map();events=[];waiters=[];
  constructor(config,compiler=executable) {
    this.adapter=new FosterDebugAdapter(compiler,{type:'foster',request:'launch',name:'test',program:file,cwd:fixture,args:[],env:{},...config});
    this.adapter.onDidSendMessage(message=>{
      if(message.type==='response') {const pending=this.pending.get(message.request_seq);this.pending.delete(message.request_seq);if(!pending)throw Error('Unexpected response');message.success?pending.resolve(message.body):pending.reject(Error(message.message));}
      else {const index=this.waiters.findIndex(w=>w.name===message.event);if(index>=0)this.waiters.splice(index,1)[0].resolve(message.body);else this.events.push(message);}
    });
  }
  request(command,args={}) {const seq=++this.sequence;return new Promise((resolve,reject)=>{this.pending.set(seq,{resolve,reject});this.adapter.handleMessage({seq,type:'request',command,arguments:args});});}
  event(name) {const index=this.events.findIndex(e=>e.event===name);if(index>=0)return Promise.resolve(this.events.splice(index,1)[0].body);return new Promise(resolve=>this.waiters.push({name,resolve}));}
  async initialize(){await this.request('initialize',{linesStartAt1:true,columnsStartAt1:true,pathFormat:'path'});await this.event('initialized');}
  dispose(){this.adapter.dispose();}
}

test('active package rejects unrelated workspace roots and accepts nested manifests',()=>{
  const folder={uri:{fsPath:root}};
  vscodeMock.workspace.workspaceFolders=[folder];
  vscodeMock.workspace.getWorkspaceFolder=()=>folder;
  assert.throws(()=>activeTarget(true),/workspace root is not a Foster package/);
  assert.deepEqual(packageTaskTargets(),[]);
  const project=path.join(fixture,'nested');fs.mkdirSync(project);fs.writeFileSync(path.join(project,'foster.toml'),'');
  vscodeMock.window.activeTextEditor={document:{languageId:'toml',isUntitled:false,uri:{scheme:'file',fsPath:path.join(project,'foster.toml')}}};
  assert.equal(activeTarget(true).program,project);
  assert.deepEqual(packageTaskTargets().map(target=>target.program),[project]);
  vscodeMock.window.activeTextEditor=undefined;
  vscodeMock.workspace.workspaceFolders=[{uri:{fsPath:fixture}}];
  assert.equal(activeTarget(true).program,fixture);
  vscodeMock.workspace.workspaceFolders=[{uri:{fsPath:project}}];
  assert.equal(activeTarget(true).program,project);
  assert.deepEqual(packageTaskTargets().map(target=>target.program),[project]);
});

test('package discovery prefers manifest above src/main.fos and preserves argument boundaries',()=>{
  const project=path.join(fixture,'project space');fs.mkdirSync(path.join(project,'src'),{recursive:true});fs.writeFileSync(path.join(project,'foster.toml'),'');fs.writeFileSync(path.join(project,'src/main.fos'),'');
  assert.equal(findPackageRoot(path.join(project,'src/main.fos'),project),project);
  const options=launchOptions({program:'project space',args:['two words','--flag','a"b','$(not-a-shell)'],optimize:false},fixture);
  assert.deepEqual(commandArguments('run',options),['run',project,'--no-optimize','--','two words','--flag','a"b','$(not-a-shell)']);
  assert.equal(options.cwd,project);assert.equal(taskPath('${workspaceFolder}/src',project),project+'/src');
  assert.throws(()=>launchOptions({program:file,args:[1]},fixture),/strings/);
  assert.throws(()=>launchOptions({program:file,env:{TEST:1}},fixture),/string/);
  assert.throws(()=>taskPath('${workspaceFolder}'),/resolve/);
  assert.throws(()=>commandArguments('test',options),/run tasks/);
});

test('inline adapter forwards a real debug session and drains stdout before termination',{timeout:20000},async t=>{
  const client=new Client({stopOnEntry:true});t.after(()=>client.dispose());await client.initialize();await client.request('launch',{stopOnEntry:true});await client.request('configurationDone');await client.event('stopped');
  let frames=await client.request('stackTrace',{threadId:1});assert.equal(frames.stackFrames[0].line,2);
  await client.request('next',{threadId:1});await client.event('stopped');frames=await client.request('stackTrace',{threadId:1});assert.equal(frames.stackFrames[0].line,3);
  assert.equal((await client.request('evaluate',{frameId:frames.stackFrames[0].id,expression:'value'})).result,'42');
  await client.request('continue',{threadId:1});const exit=await client.event('exited');assert.equal(exit.exitCode,0,JSON.stringify(client.events));await client.event('terminated');
  assert.ok(client.events.some(e=>e.event==='output' && e.body.category==='stdout' && e.body.output.includes('42')));
  assert.ok(!client.events.some(e=>e.event==='output' && e.body.category==='stderr'));
});

test('run without debugging uses the launch lifecycle',{timeout:20000},async t=>{
  const client=new Client({noDebug:true});t.after(()=>client.dispose());await client.initialize();await client.request('launch');await client.request('configurationDone');assert.equal((await client.event('exited')).exitCode,0);await client.event('terminated');
  assert.ok(client.events.some(e=>e.event==='output' && e.body.output.includes('42')));
});

test('missing compiler returns a launch failure instead of hanging',{timeout:10000},async t=>{
  const client=new Client({},path.join(fixture,'missing-compiler'));t.after(()=>client.dispose());await assert.rejects(client.initialize());assert.equal((await client.event('exited')).exitCode,1);await client.event('terminated');
});

test('disconnecting a paused session terminates the child',{timeout:20000},async t=>{
  const client=new Client({stopOnEntry:true});t.after(()=>client.dispose());await client.initialize();await client.request('launch',{stopOnEntry:true});await client.request('configurationDone');await client.event('stopped');await client.request('disconnect');await client.event('terminated');
});

test('launch arguments containing spaces reach main unchanged',{timeout:120000},async t=>{
  const program=path.join(fixture,'arguments.fos');fs.copyFileSync(path.join(root,'tests/fixtures/programs/arguments.fos'),program);
  const client=new Client({program,args:['two words; $(literal)'],stopOnEntry:false});t.after(()=>client.dispose());await client.initialize();await client.request('launch',{stopOnEntry:false});await client.request('configurationDone');assert.equal((await client.event('exited')).exitCode,0);await client.event('terminated');
  assert.ok(client.events.some(e=>e.event==='output' && e.body.category==='stdout' && e.body.output.includes('two words; $(literal)')));
});

test('runtime failure preserves the program exit code',{timeout:20000},async t=>{
  const program=path.join(fixture,'failure.fos');fs.writeFileSync(program,'func main() {\n    assert(false, "adapter failure")\n}\n');
  const client=new Client({program,stopOnEntry:false});t.after(()=>client.dispose());await client.initialize();await client.request('launch',{stopOnEntry:false});await client.request('configurationDone');assert.equal((await client.event('exited')).exitCode,1);await client.event('terminated');
  assert.ok(client.events.some(e=>e.event==='output' && e.body.category==='stderr' && e.body.output.includes('adapter failure')));
});
