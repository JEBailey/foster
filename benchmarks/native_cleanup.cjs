// Paired comparison of the frozen pre-cleanup compiler and current native compiler.
const fs = require('node:fs');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const {performance} = require('node:perf_hooks');
const {createHash} = require('node:crypto');
const assert = require('node:assert/strict');
const repo = path.resolve(__dirname, '..');
const directory = path.join(repo, 'target/native-cleanup');
const oldReport = JSON.parse(fs.readFileSync(path.join(__dirname,'results/native_from_bytecode_large.json')));
const programs = JSON.parse(fs.readFileSync(path.join(__dirname,'results/native_from_bytecode_programs.json')));
oldReport.cases.push(...programs.cases.map(row=>({...row,family:row.name,variants:{ssa:{args:['build',row.source]}}})));
const compilers = {before:path.join(directory,'compiler-before.exe'), after:path.join(directory,'compiler-after.exe')};
if(process.argv[2]!=='--retime-early') fs.copyFileSync(path.join(repo,'target/release/foster.exe'),compilers.after);
const destination = path.join(__dirname,'results/native_cleanup.json');
const hash = file => createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const median = values => [...values].sort((a,b)=>a-b)[Math.floor(values.length/2)];
function invoke(exe,args) {
  const start=performance.now();
  const result=spawnSync(exe,args,{cwd:repo,encoding:'utf8',timeout:180000,maxBuffer:8*1024*1024});
  assert.equal(result.status,0,result.stderr||String(result.error));
  return {ms:performance.now()-start,stdout:result.stdout.trim()};
}
const report=process.argv[2]==='--resume'?JSON.parse(fs.readFileSync(destination)):{date:new Date().toISOString(),cpu:oldReport.cpu,repeats:5,compilerHashes:Object.fromEntries(Object.entries(compilers).map(([k,v])=>[k,hash(v)])),
  note:'Default optimized native route in both compilers. Identical source hashes and expected results from the larger comparison. Three alternating build samples and five alternating runtime samples after warmup. Includes process startup and cleanup.',cases:[]};
function save() { fs.writeFileSync(destination,JSON.stringify(report,null,2)+'\n'); }
if(process.argv[2]==='--retime-early') {
  const measured=JSON.parse(fs.readFileSync(destination));
  assert.deepEqual(measured.compilerHashes,report.compilerHashes);
  for(const row of measured.cases.slice(0,9)) {
    for(const entry of Object.values(row.variants)) {
      entry.initialRuntimeMs=entry.runtimeMs; entry.initialBuildMs=entry.buildMs;
      entry.runtimeMs=[]; entry.buildMs=[];
      assert.equal(invoke(entry.output,[]).stdout,row.expected);
    }
    for(let sample=0;sample<3;sample++) for(const variant of sample%2?['after','before']:['before','after']) {
      const entry=row.variants[variant]; const args=[...entry.args];
      args[args.indexOf('-o')+1]=entry.output+`-retime-${sample}.exe`;
      entry.buildMs.push(invoke(compilers[variant],args).ms);
    }
    for(let sample=0;sample<5;sample++) for(const variant of sample%2?['after','before']:['before','after']) {
      const entry=row.variants[variant]; const run=invoke(entry.output,[]);
      assert.equal(run.stdout,row.expected); entry.runtimeMs.push(run.ms);
    }
    for(const entry of Object.values(row.variants)) {
      entry.medianRuntimeMs=median(entry.runtimeMs); entry.medianBuildMs=median(entry.buildMs);
    }
    row.runtimeSpeedup=row.variants.before.medianRuntimeMs/row.variants.after.medianRuntimeMs;
    row.buildTimeRatio=row.variants.after.medianBuildMs/row.variants.before.medianBuildMs;
    row.retimedAfterBackgroundBuilds=true;
    fs.writeFileSync(destination,JSON.stringify(measured,null,2)+'\n');
    console.log(JSON.stringify({name:row.name,speedup:row.runtimeSpeedup,buildRatio:row.buildTimeRatio}));
  }
  measured.note+=' First nine rows were measured again after unrelated background builds ended; their original samples are retained.';
  fs.writeFileSync(destination,JSON.stringify(measured,null,2)+'\n');
  process.exit(0);
}
for(const original of oldReport.cases) {
  if(report.cases.some(row=>row.name===original.name)) continue;
  const source=original.variants.ssa.args[1];
  assert.equal(hash(fs.statSync(source).isDirectory()?path.join(source,'src/main.fos'):source),original.sourceSha256);
  const row={name:original.name,family:original.family,sourceSha256:original.sourceSha256,expected:original.expected,variants:{}};
  report.cases.push(row);
  for(const variant of ['before','after']) {
    const output=path.join(directory,original.name+'-'+variant+'.exe');
    const args=['build',source,'--native','-o',output];
    invoke(compilers[variant],args);
    assert.equal(invoke(output,[]).stdout,original.expected);
    row.variants[variant]={output,args,buildMs:[],runtimeMs:[],executableBytes:fs.statSync(output).size};
    if(['scalar_1x','record_reuse_1x','list_push_1x'].includes(row.name)) {
      row.variants[variant].bytecodeHashes={};
      for(const optimize of [true,false]) {
        const bytecode=output+(optimize?'.fbc':'.noopt.fbc');
        invoke(compilers[variant],['build',source,'-o',bytecode,...(optimize?[]:['--no-optimize'])]);
        row.variants[variant].bytecodeHashes[String(optimize)]=hash(bytecode);
      }
    }
  }
  for(let sample=0;sample<3;sample++) for(const variant of sample%2?['after','before']:['before','after']) {
    const entry=row.variants[variant];
    const args=[...entry.args]; args[args.indexOf('-o')+1]=entry.output+`-build-${sample}.exe`;
    entry.buildMs.push(invoke(compilers[variant],args).ms);
  }
  for(let sample=0;sample<5;sample++) for(const variant of sample%2?['after','before']:['before','after']) {
    const entry=row.variants[variant]; const result=invoke(entry.output,[]);
    assert.equal(result.stdout,original.expected); entry.runtimeMs.push(result.ms);
  }
  for(const entry of Object.values(row.variants)) {
    entry.medianBuildMs=median(entry.buildMs); entry.medianRuntimeMs=median(entry.runtimeMs);
  }
  row.runtimeSpeedup=row.variants.before.medianRuntimeMs/row.variants.after.medianRuntimeMs;
  row.buildTimeRatio=row.variants.after.medianBuildMs/row.variants.before.medianBuildMs;
  if(row.variants.before.bytecodeHashes) assert.deepEqual(row.variants.before.bytecodeHashes,row.variants.after.bytecodeHashes);
  save(); console.log(JSON.stringify({name:row.name,speedup:row.runtimeSpeedup,buildRatio:row.buildTimeRatio}));
}
save();
