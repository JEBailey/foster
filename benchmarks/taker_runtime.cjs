// Windows runtime comparison. Requires Java 21+, a release Foster compiler,
// and sibling taker/taker and taker_foster repositories.
const fs=require('node:fs'),path=require('node:path'),os=require('node:os'),crypto=require('node:crypto');
const {spawn}=require('node:child_process');
const {performance}=require('node:perf_hooks');
const assert=require('node:assert/strict');
const repo=path.resolve(__dirname,'..'),exe=path.join(repo,'target/release/foster.exe');
const output=path.join(repo,'target','taker-runtime-'+Date.now());
const consumer=path.join(output,'consumer'),classes=path.join(output,'classes');
for(const dir of [path.join(consumer,'src'),classes])fs.mkdirSync(dir,{recursive:true});
const dependency=path.relative(consumer,path.resolve(repo,'../taker_foster')).replaceAll('\\','/');
fs.writeFileSync(path.join(consumer,'foster.toml'),`[package]\nname = "runtime-benchmark"\n[dependencies]\ntaker = { path = "${dependency}" }\n`);
fs.copyFileSync(path.join(__dirname,'taker/main.fos'),path.join(consumer,'src/main.fos'));
function run(command,args,log,env=process.env){return new Promise((resolve,reject)=>{const start=performance.now();let text='';const child=spawn(command,args,{cwd:repo,env,stdio:['ignore','pipe','pipe']});const timer=setTimeout(()=>child.kill(),180000);child.on('error',reject);child.stdout.on('data',b=>text+=b);child.stderr.on('data',b=>text+=b);child.on('close',code=>{clearTimeout(timer);if(log)fs.writeFileSync(path.join(output,log),text);resolve({code,text,wallMs:performance.now()-start});});});}
function javaFiles(dir){return fs.readdirSync(dir,{withFileTypes:true}).flatMap(e=>e.isDirectory()?javaFiles(path.join(dir,e.name)):e.name.endsWith('.java')?[path.join(dir,e.name)]:[]);}
const median=xs=>{const a=[...xs].sort((a,b)=>a-b),i=Math.floor(a.length/2);return a.length%2?a[i]:(a[i-1]+a[i])/2;};
(async()=>{
 console.log('Output:',output);
 const sources=javaFiles(path.resolve(repo,'../taker/taker/src/main/java'));
 let step=await run('javac',['--release','21','-d',classes,...sources,path.join(__dirname,'taker/TakerSpeed.java')],'java-build.log');assert.equal(step.code,0,step.text);
 const bytecode=path.join(output,'benchmark.fbc');
 step=await run(exe,['build',consumer,'-o',bytecode],'foster-build.log');assert.equal(step.code,0,step.text);
 const native=await run(exe,['build',consumer,'--native','-o',path.join(output,'benchmark.exe')],'native-build.log',{...process.env,RUSTC:'C:/Users/jason/.cargo/bin/rustc.exe',FOSTER_NATIVE_CACHE_DIR:path.join(repo,'target/native-runtime-cache')});
 const results={timestamp:new Date().toISOString(),cpu:os.cpus()[0].model.trim(),platform:os.platform(),compilerSha256:crypto.createHash('sha256').update(fs.readFileSync(exe)).digest('hex'),javaVersion:(await run('java',['-version'])).text,nativeBuild:{code:native.code,error:native.text.split(/\r?\n/).filter(l=>l.startsWith('error:')).join('\n')},forks:3,samples:[]};
 const backends=['java','foster-vm'];if(native.code===0)backends.push('foster-native');
 const save=()=>fs.writeFileSync(path.join(output,'results.json'),JSON.stringify(results,null,2)+'\n');
 for(let fork=0;fork<results.forks;fork++){
  const order=fork%2?[...backends].reverse():backends;
  for(const backend of order){
   const command=backend==='java'?'java':backend==='foster-vm'?exe:path.join(output,'benchmark.exe');
   const args=backend==='java'?['-Xms256m','-Xmx256m','-cp',classes,'TakerSpeed']:backend==='foster-vm'?['run',bytecode]:[];
   const measured=await run(command,args,`${backend}-${fork}.log`);assert.equal(measured.code,0,measured.text);
   const rows=measured.text.split(/\r?\n/).filter(l=>/^[a-z0-9_]+,\d+,\d+,\d+$/.test(l)).map(line=>{const [workload,iterations,elapsedNs,checksum]=line.split(',');return {fork,backend,workload,iterations:Number(iterations),elapsedNs:Number(elapsedNs),checksum:Number(checksum),nsPerParse:Number(elapsedNs)/Number(iterations)};});
   assert.equal(rows.length,20,measured.text);results.samples.push(...rows);save();
   console.log(JSON.stringify({fork,backend,wallSeconds:measured.wallMs/1000,nsPerParse:Object.fromEntries(['literal16','integer9','scan256','trimmed_integer'].map(w=>[w,median(rows.filter(r=>r.workload===w).map(r=>r.nsPerParse))]))}));
  }
 }
 results.summary=[];
 for(const workload of ['literal16','integer9','scan256','trimmed_integer'])for(const backend of backends){const all=results.samples.filter(s=>s.workload===workload&&s.backend===backend);const forks=Array.from({length:3},(_,i)=>median(all.filter(s=>s.fork===i).map(s=>s.nsPerParse)));results.summary.push({workload,backend,medianNs:median(forks),minForkMedianNs:Math.min(...forks),maxForkMedianNs:Math.max(...forks),forkMedianNs:forks});}
 save();console.log(JSON.stringify(results.summary,null,2));
})().catch(e=>{console.error(e);process.exitCode=1;});
