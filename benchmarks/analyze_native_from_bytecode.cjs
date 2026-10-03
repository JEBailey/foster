const fs=require('node:fs'),path=require('node:path');
const source=process.argv[2]||'benchmarks/results/native_from_bytecode_large.json';
const report=JSON.parse(fs.readFileSync(source,'utf8'));
const median=xs=>{const a=[...xs].sort((a,b)=>a-b);return a.length%2?a[(a.length-1)/2]:(a[a.length/2-1]+a[a.length/2])/2;};
const geometric=xs=>Math.exp(xs.reduce((s,x)=>s+Math.log(x),0)/xs.length);
let seed=1729;const random=()=>{seed=(Math.imul(seed,1664525)+1013904223)>>>0;return seed/4294967296;};
const rows=report.cases.map(row=>{
 const a=row.variants.ssa,b=row.variants.bytecode;
 if(!a?.runtimeMs.length||!b?.runtimeMs.length)return {name:row.name,family:row.family,supported:false,error:b?.warmBuild.stderr||a?.warmBuild.stderr};
 const bootstrap=[];
 for(let k=0;k<5000;k++) {const x=[],y=[];for(let i=0;i<a.runtimeMs.length;i++){const j=Math.floor(random()*a.runtimeMs.length);x.push(a.runtimeMs[j]);y.push(b.runtimeMs[j]);}bootstrap.push(median(x)/median(y));}
 bootstrap.sort((x,y)=>x-y);
 return {name:row.name,family:row.family,supported:true,speedup:row.runtimeSpeedup,ci95:[bootstrap[125],bootstrap[4875]],defaultMs:a.medianRuntimeMs,bytecodeMs:b.medianRuntimeMs,buildRatio:row.buildTimeRatio,sizeRatio:b.executableBytes/a.executableBytes};
});
const families=[...new Set(rows.map(r=>r.family))].map(family=>{
 const all=rows.filter(r=>r.family===family),ok=all.filter(r=>r.supported);
 return {family,passed:ok.length,total:all.length,speedup:ok.length?geometric(ok.map(r=>r.speedup)):null,buildRatio:ok.length?geometric(ok.map(r=>r.buildRatio)):null,sizeRatio:ok.length?geometric(ok.map(r=>r.sizeRatio)):null};
});
const complete=families.filter(f=>f.passed===f.total);
const summary={supported:rows.filter(r=>r.supported).length,total:rows.length,familyBalancedRuntimeSpeedup:geometric(complete.map(f=>f.speedup)),familyBalancedBuildRatio:geometric(complete.map(f=>f.buildRatio)),families,rows};
fs.writeFileSync(source.replace(/\.json$/,'.analysis.json'),JSON.stringify(summary,null,2)+'\n');
console.log(JSON.stringify(summary,null,2));
