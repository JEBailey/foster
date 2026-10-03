// Diagnostic copies only: bypass generated cancellation calls, never production binaries.
const fs=require('node:fs'),cp=require('node:child_process'),assert=require('node:assert/strict');
const dir='target/native-route-analysis',raw=JSON.parse(fs.readFileSync('benchmarks/results/native_from_bytecode_large.json'));
function coff(data) {
 const count=data.readUInt16LE(2),symptr=data.readUInt32LE(8),nsyms=data.readUInt32LE(12),strings=symptr+18*nsyms;
 const sections=[];for(let i=0;i<count;i++){const o=20+i*40;sections.push({name:data.subarray(o,o+8).toString().replace(/\0.*$/,''),size:data.readUInt32LE(o+16),raw:data.readUInt32LE(o+20),relocs:data.readUInt32LE(o+24),nrelocs:data.readUInt16LE(o+32)});}
 const symbols=[];for(let i=0;i<nsyms;i++){const o=symptr+i*18;let name;if(data.readUInt32LE(o)===0){const p=strings+data.readUInt32LE(o+4);name=data.subarray(p,data.indexOf(0,p)).toString();}else name=data.subarray(o,o+8).toString().replace(/\0.*$/,'');symbols[i]={name,value:data.readUInt32LE(o+8),section:data.readInt16LE(o+12)};i+=data[o+17];}
 return {sections,symbols};
}
function bypass(source,name,route) {
 const object=fs.readFileSync(`${dir}/${name}-${route}.obj`),exe=fs.readFileSync(source),{sections,symbols}=coff(object);
 let patched=0;const matches=[];
 for(const sym of symbols.filter(s=>s?.name.startsWith('foster_fn_') || s?.name==='foster_native_entry')) {
  const sec=sections[sym.section-1],end=Math.min(sec.size,...symbols.filter(s=>s?.section===sym.section&&s.value>sym.value).map(s=>s.value));
  const body=object.subarray(sec.raw+sym.value,sec.raw+end),mask=Buffer.alloc(body.length,1),polls=[];
  for(let i=0;i<sec.nrelocs;i++) {const o=sec.relocs+i*10,offset=object.readUInt32LE(o),target=symbols[object.readUInt32LE(o+4)]?.name;if(offset<sym.value||offset>=end)continue;mask.fill(0,offset-sym.value,Math.min(offset-sym.value+4,mask.length));if(target==='.refptr.foster_rt_v5_cancellation_point')polls.push(offset-sym.value+4);}
  const anchor=body.subarray(0,Math.min(16,body.length));assert.ok(mask.subarray(0,anchor.length).every(x=>x===1));
  let found=-1,candidate=-1,count=0;while((candidate=exe.indexOf(anchor,candidate+1))>=0) {if(candidate+body.length>exe.length)continue;let equal=true;for(let i=0;i<body.length;i++)if(mask[i]&&body[i]!==exe[candidate+i]){equal=false;break;}if(equal){found=candidate;count++;}}
  assert.equal(count,1,`${name} ${route} ${sym.name}: expected one exact relocation-masked machine-code match`);
  for(const offset of polls) {
   let length;if(body[offset]===0xff&&(body[offset+1]&0xf8)===0xd0)length=2;else if(body[offset]===0x41&&body[offset+1]===0xff&&(body[offset+2]&0xf8)===0xd0)length=3;else throw new Error(`poll call encoding changed at ${sym.name}+${offset}`);
   exe[found+offset]=0x31;exe[found+offset+1]=0xc0;if(length===3)exe[found+offset+2]=0x90;patched++;
  }
  matches.push({symbol:sym.name,bytes:body.length,polls:polls.length});
 }
 assert.ok(patched>0);const output=`${dir}/${name}-${route}-NO-POLLS-DIAGNOSTIC.exe`;fs.writeFileSync(output,exe);return {output,patched,matches};
}
const report={compilerSha256:raw.compilerSha256,repeats:5,warmups:1,note:'Diagnostic copies of already benchmarked executables. Only cancellation call instructions in verified matching native function bodies are replaced by xor eax,eax (plus padding). This disables cancellation and failure polling and is not a proposed production change. Original executables and compiler sources remain untouched.',cases:[]};
const chosen=['branch_counter_4x','enum_dispatch_4x','fibonacci_32','scalar_cse_8000000','scalar_4x','list_push_4x'];
for(const name of chosen) {
 const original=raw.cases.find(c=>c.name===name),row={name,expected:original.expected,variants:{}};report.cases.push(row);
 for(const route of ['ssa','bytecode'])row.variants[route]={...bypass(original.variants[route].output,name,route),runtimeMs:[],normalRuntimeMs:[],original:original.variants[route].output};
 const order=[['ssa','normal'],['bytecode','normal'],['ssa','bypass'],['bytecode','bypass']];
 for(let sample=0;sample<6;sample++) for(const [route,mode] of sample%2?[...order].reverse():order) {
  const v=row.variants[route],start=performance.now(),r=cp.spawnSync(mode==='normal'?v.original:v.output,[],{encoding:'utf8',timeout:30000}),ms=performance.now()-start;
  assert.equal(r.status,0,r.stderr);assert.equal(r.stdout.trim(),row.expected);
  if(sample)(mode==='normal'?v.normalRuntimeMs:v.runtimeMs).push(ms);
 }
 for(const v of Object.values(row.variants)) {
  v.medianMs=[...v.runtimeMs].sort((a,b)=>a-b)[2];
  v.normalMedianMs=[...v.normalRuntimeMs].sort((a,b)=>a-b)[2];
 }
 row.speedup=row.variants.ssa.medianMs/row.variants.bytecode.medianMs;
 row.normalSpeedup=row.variants.ssa.normalMedianMs/row.variants.bytecode.normalMedianMs;
 fs.writeFileSync('benchmarks/results/native_route_poll_ablation.json',JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify({name,speedup:row.speedup,normalSpeedup:row.normalSpeedup,ssaMs:row.variants.ssa.medianMs,bytecodeMs:row.variants.bytecode.medianMs,polls:Object.values(row.variants).map(v=>v.patched)}));
}
