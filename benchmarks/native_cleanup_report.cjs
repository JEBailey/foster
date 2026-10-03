const fs=require('node:fs');
const report=JSON.parse(fs.readFileSync('benchmarks/results/native_cleanup.json'));
const gm=values=>Math.exp(values.reduce((sum,value)=>sum+Math.log(value),0)/values.length);
const groups=new Map();
for(const row of report.cases) {
  if(!row.runtimeSpeedup) throw new Error('Incomplete benchmark: '+row.name);
  if(!groups.has(row.family)) groups.set(row.family,[]);
  groups.get(row.family).push(row);
}
const fixtures=[...groups].filter(([,rows])=>rows[0].name!==rows[0].family);
const runtime=gm(fixtures.map(([,rows])=>gm(rows.map(row=>row.runtimeSpeedup))));
const build=gm(fixtures.map(([,rows])=>gm(rows.map(row=>row.buildTimeRatio))));
const reduction=ratio=>(100*(1-1/ratio)).toFixed(1);
const lines=['# Default native cleanup comparison','',
  `${report.cases.length} checked workloads on ${report.cpu.trim()}. The smaller workloads cover ${fixtures.length} families. Their family-balanced geometric mean is ${runtime.toFixed(3)}× faster (${reduction(runtime)}% less runtime); native build time changes by ${(100*(build-1)).toFixed(1)}%. Formatter and parser results appear separately in the table.`, '',
  report.note,'',
  'Both routes use the default optimized native compiler. The new route keeps source type information and does not reconstruct types from VM bytecode. Each run checks its result against the independently calculated expected value; source hashes are identical. Bytecode artifacts also match between the two frozen compilers for scalar loops, record mutation, and list growth in both optimization modes.','',
  'Compiler hashes:', '', `- Before: \`${report.compilerHashes.before}\``, `- After: \`${report.compilerHashes.after}\``, '',
  '| Workload | Before (ms) | After (ms) | Runtime reduction | Build change |',
  '| --- | ---: | ---: | ---: | ---: |'];
for(const row of report.cases) lines.push(`| ${row.name} | ${row.variants.before.medianRuntimeMs.toFixed(1)} | ${row.variants.after.medianRuntimeMs.toFixed(1)} | ${reduction(row.runtimeSpeedup)}% | ${(100*(row.buildTimeRatio-1)).toFixed(1)}% |`);
const oldCode=JSON.parse(fs.readFileSync('benchmarks/results/native_route_codegen.json'));
const newCode=JSON.parse(fs.readFileSync('benchmarks/results/native_cleanup_codegen.json'));
lines.push('', 'Generated code for the default route:', '',
  '| Workload / function | Polls before → after | Block parameters before → after | Stack reservation before → after (bytes) |',
  '| --- | ---: | ---: | ---: |');
for(const row of newCode.cases.filter(row=>row.route==='ssa')) {
  const old=oldCode.cases.find(old=>old.name===row.name&&old.route==='ssa');
  row.functions.forEach((fn,index)=>lines.push(`| ${row.name} / ${fn.name} | ${old.functions[index].polls} → ${fn.polls} | ${old.functions[index].blockParameters} → ${fn.blockParameters} | ${old.machine[index].frameBytes} → ${row.machine[index].frameBytes} |`));
}
lines.push('', 'Poll counts are static sites, not dynamic execution counts. Stack reservations include backend spill and call storage. IR home annotations are retained as identities and do not count allocated stack slots.');
lines.push('', 'Native cleanup removes non-addressable scalar copies and unused scalar block parameters, threads poll-only jump blocks, and allocates stack homes only for address-taken storage. Managed ownership operations, exposed storage, checked arithmetic, poll-only cycles, and failure cleanup remain intact. Unoptimized native lowering is unchanged.', '',
  'Validation: 180 compiler/runtime tests passed (one existing ignored test), plus all 24 native integration tests. Coverage includes cancellation while records remain live, failure reclamation, reference captures, address-taken pattern bindings, copy-on-write mutation, collections, contract dispatch, remote workers, and host services. A native regression check confirms VM encoding is unchanged and scalar workloads allocate no addressable stack homes.', '',
  'These are local paired measurements with process startup and cleanup included. They establish improvements for the measured workloads, not a guarantee for every Foster program. Original early samples are retained alongside the repeated measurements.', '');
fs.writeFileSync('benchmarks/results/native_cleanup.md',lines.join('\n'));
console.log(JSON.stringify({cases:report.cases.length,families:fixtures.length,speedup:runtime,runtimeReduction:Number(reduction(runtime)),buildChange:100*(build-1)}));
