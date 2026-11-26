'use strict';
// Attribute final encoded wasm bodies to the compiler's first-retaining edges.
// Capture with RELAX_WASM_EXPORT_FNLIST=<prefix>, then:
// node tools/wasm-export/analyze-closure.js <linked.wasm> <prefix>
// No execution samples are used as liveness evidence. Category compression is
// independent (br11/lgwin24), so compressed categories are not additive savings.
const fs=require('node:fs'),zlib=require('node:zlib'),assert=require('node:assert/strict');
const [modulePath,prefix]=process.argv.slice(2);
if(!modulePath||!prefix)throw Error('usage: node analyze-closure.js <linked.wasm> <functions-tsv-prefix>');
const bytes=fs.readFileSync(modulePath);
assert(WebAssembly.validate(bytes),'invalid wasm');
const tsv=file=>{const lines=fs.readFileSync(file,'utf8').trimEnd().split('\n'),keys=lines.shift().split('\t');return lines.filter(Boolean).map(l=>Object.fromEntries(l.split('\t').map((v,i)=>[keys[i],v])));};
const rows=tsv(prefix+'.reachability'),vtables=tsv(prefix+'.vtables');
const tableBySymbol=new Map(vtables.map(v=>[v.symbol,v])),methodEvidence=new Map();
for(const edge of tsv(prefix+'.vtable-methods')) {
  const table=tableBySymbol.get(edge.symbol);assert(table,'method without live table');
  if(!methodEvidence.has(edge.slot))methodEvidence.set(edge.slot,new Set());
  methodEvidence.get(edge.slot).add(table.evidence);
}
const br=b=>zlib.brotliCompressSync(b,{params:{[zlib.constants.BROTLI_PARAM_QUALITY]:11,[zlib.constants.BROTLI_PARAM_LGWIN]:24,[zlib.constants.BROTLI_PARAM_SIZE_HINT]:b.length}}).length;
const size=b=>({raw:b.length,gzip9:zlib.gzipSync(b,{level:9}).length,br11:br(b)});
let p=8;
const leb=()=>{let n=0,s=0,b;do{assert(p<bytes.length);b=bytes[p++];n+=(b&127)*2**s;s+=7;assert(s<=35);}while(b&128);return n;};
const sections={},bodies=[];
while(p<bytes.length){const id=bytes[p++],n=leb(),start=p,end=p+n;assert(end<=bytes.length);sections[id]={start,bytes:n};if(id===10){const count=leb();for(let i=0;i<count;i++){const len=leb();bodies.push({offset:p,bytes:len});p+=len;}assert.equal(p,end);}p=end;}
const bodyMap=new Map(bodies.map(b=>[b.offset,b])),used=new Set(),groups=new Map(),crates=new Map(),functions=[];
const add=(group,row)=>{if(!groups.has(group))groups.set(group,[]);groups.get(group).push(row);};
for(const row of rows){
  row.offset=Number(row.offset);row.bytes=Number(row.bytes);row.slot=Number(row.slot);
  if(row.bytes){assert.equal(bodyMap.get(row.offset)?.bytes,row.bytes,`body mismatch at ${row.offset}`);assert(!used.has(row.offset),'duplicate body ownership');used.add(row.offset);}
  add(row.edge,row);
  if(row.generic==='true')add('generic instances (overlapping)',row);
  const method=methodEvidence.get(String(row.slot));
  if(method)add(method.has('code-address')?'all vtable methods with code evidence (overlapping)':method.has('initial-data-pointer')?'all vtable methods with data-only evidence (overlapping)':'all vtable methods without evidence (overlapping)',row);
  const c=crates.get(row.crate)||{crate:row.crate,raw:0,functions:0};c.raw+=row.bytes;c.functions++;crates.set(row.crate,c);
  functions.push({slot:row.slot,raw:row.bytes,crate:row.crate,key:row.key,edge:row.edge,parent:row.parent,via:row.via});
}
groups.set('backend helpers and ABI wrappers',bodies.filter(b=>!used.has(b.offset)));
const slots=new Set(rows.map(r=>String(r.slot)));
for(const slot of methodEvidence.keys())assert(slots.has(slot),`live vtable method ${slot} absent from closure`);
const categories=Object.fromEntries([...groups].map(([name,rs])=>{const b=Buffer.concat(rs.filter(r=>r.bytes).sort((a,b)=>a.offset-b.offset).map(r=>bytes.subarray(r.offset,r.offset+r.bytes)));return [name,{functions:rs.length,emitted:rs.filter(r=>r.bytes).length,...size(b)}];}));
const code=bytes.subarray(sections[10].start,sections[10].start+sections[10].bytes);
const totalBodyBytes=bodies.reduce((n,b)=>n+b.bytes,0);
assert.equal(Object.entries(categories).filter(([k])=>!k.includes('(overlapping)')).reduce((n,[,v])=>n+v.raw,0),totalBodyBytes);
const evidence={};for(const v of vtables){assert(['code-address','initial-data-pointer','unproven'].includes(v.evidence));evidence[v.evidence]=(evidence[v.evidence]||0)+1;}
console.log(JSON.stringify({module:modulePath,valid:true,module_size:size(bytes),code_section:size(code),body_bytes:totalBodyBytes,code_framing_bytes:code.length-totalBodyBytes,categories,vtables:{total:vtables.length,evidence,without_liveness_evidence:evidence.unproven||0},top_crates:[...crates.values()].sort((a,b)=>b.raw-a.raw).slice(0,30),top_functions:functions.sort((a,b)=>b.raw-a.raw).slice(0,30),notes:['First-retaining edges partition source bodies; generic instances overlap.','Groups are independently compressed in module order; br11 values do not add to module br11.','Body bytes include final call/type LEB relocations, exclude code framing; aliases own zero bytes.','A code-address includes unsizing/construction that materializes the table. Data-only tables are live under the same retained-data rule.']},null,2));
