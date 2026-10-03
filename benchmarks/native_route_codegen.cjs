// Inspect optimized native IR and emitted machine code for both routes.
// First build/run native_route_analysis.rs as described in the analysis report.
const fs=require('node:fs'),cp=require('node:child_process'),crypto=require('node:crypto');
const directory=process.argv[2]||'target/native-route-analysis';
const source=JSON.parse(fs.readFileSync('benchmarks/results/native_from_bytecode_large.json'));
const homes=fs.readFileSync(`${directory}/homes.jsonl`,'utf8').trim().split(/\r?\n/).map(JSON.parse);
const names=['branch_counter_4x','enum_dispatch_4x','fibonacci_32','scalar_cse_8000000','scalar_4x','list_push_4x'];
const metrics=[];
for(const name of names)for(const route of ['ssa','bytecode'].filter(route=>fs.existsSync(`${directory}/${name}-${route}.ir`))) {
 const ir=fs.readFileSync(`${directory}/${name}-${route}.ir`,'utf8');
 const object=fs.readFileSync(`${directory}/${name}-${route}.obj`);
 const process=cp.spawnSync('C:/Program Files/LLVM/bin/llvm-objdump.exe',['-dr',`${directory}/${name}-${route}.obj`],{encoding:'utf8',maxBuffer:8*1024*1024});if(process.status!==0)throw new Error(process.stderr);
 const dump=process.stdout;fs.writeFileSync(`${directory}/${name}-${route}.asm`,dump);
 const functions=ir.split(/(?=; function #)/).slice(1).map(text=>{
  const functionName=/^function ([^(]+)/m.exec(text)[1];
  const home=homes.find(h=>h.source===name&&h.route===route&&h.function===functionName);
  return {name:functionName,blocks:(text.match(/^  b\d+\(/gm)||[]).length,polls:(text.match(/runtime foster_rt_v5_cancellation_point/g)||[]).length,moves:(text.match(/portable Move \{/g)||[]).length,homes:home.homes,blockParameters:home.block_parameters};
 });
 const symbols=[...dump.matchAll(/^([0-9a-f]+) <([^>]+)>:/gm)];
 const machine=symbols.flatMap((m,index)=>{
  if(!m[2].startsWith('foster_fn_')&&m[2]!=='foster_native_entry')return [];
  const text=dump.slice(m.index,symbols[index+1]?.index||dump.length);
  return [{name:m[2],pollLoads:(text.match(/\.refptr\.foster_rt_v5_cancellation_point/g)||[]).length,instructions:(text.match(/^\s+[0-9a-f]+:\s+[0-9a-f]{2}\s/gm)||[]).length,stackReferences:(text.match(/\(%rsp\)|\(%rbp\)/g)||[]).length,frameBytes:Number(/subq\s+\$(\d+), %rsp/.exec(text)?.[1]||0)}];
 });
 metrics.push({name,route,irSha256:crypto.createHash('sha256').update(ir).digest('hex'),objectSha256:crypto.createHash('sha256').update(object).digest('hex'),functions,machine});
}
fs.writeFileSync(process.argv[3]||'benchmarks/results/native_route_codegen.json',JSON.stringify({compilerSha256:process.argv[4]||source.compilerSha256,note:'Optimized preparation is used for both routes. Static machine counts include cold failure paths. The homes metric counts retained IR storage identities, not allocated stack homes.',cases:metrics},null,2)+'\n');
console.log(JSON.stringify(metrics.map(r=>({name:r.name,route:r.route,functions:r.functions,machine:r.machine})),null,2));
