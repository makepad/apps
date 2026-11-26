'use strict';
// Explicit, table-driven browser surface. Unknown APIs are absent (never a
// catch-all Proxy returning no-ops). No network, browser, GPU or npm dependency.
const fs=require('node:fs'), path=require('node:path');
const {audio}=require('./audio');
const FRAME=1/60, EPOCH=1767225600;
class Clock {
  constructor(environment={}){this.now=0;this.nextId=1;this.timers=new Map();this.rafs=new Map();this.jitter=environment.jitter??true;this.program=false;this.readers=[{seed:environment.seed??1,last:0},{seed:environment.seed??1,last:0}];}
  read(){
    if(!this.jitter)return this.now;
    const state=this.readers[Number(this.program)];
    state.seed=(Math.imul(state.seed,1664525)+1013904223)>>>0;
    state.last=Math.max(state.last,this.now)+(1+(state.seed>>>24))/1000000;
    return state.last;
  }
  timer(callback,ms,repeat,args){const id=this.nextId++;const interval=Number(ms)/1000;this.timers.set(id,{callback,args,interval,repeat,next:this.now+interval});return id;}
  raf(callback){const id=this.nextId++;this.rafs.set(id,{callback,next:(Math.floor(this.now/FRAME+1e-6)+1)*FRAME});return id;}
  next(){return Math.min(...[...this.timers.values(),...this.rafs.values()].map(t=>t.next));}
  async timersDue(settle,pollId){
    const due=[...this.timers].filter(([,v])=>v.next<=this.now);
    // page.rs runs application timers before the signal poll at a shared tick.
    due.sort((a,b)=>Number(a[0]===pollId)-Number(b[0]===pollId));
    for(const [id,t] of due){if(!this.timers.has(id))continue;if(t.repeat)t.next+=t.interval;else this.timers.delete(id);t.callback(...t.args);await settle();}
  }
  async framesDue(after,settle){
    for(const [id,t] of [...this.rafs])if(t.next<=this.now){this.rafs.delete(id);t.callback(this.now*1000);after();await settle();}
  }
}
function browser(page, dimensions, clock, log, environment={}) {
  const used=new Set(); let document;
  const classLists=[];
  function target(name,properties={}) {
    const listeners=new Map();
    return Object.assign({
      addEventListener(type,fn){used.add(`${name}.addEventListener(${type})`);if(!listeners.has(type))listeners.set(type,[]);listeners.get(type).push(fn);},
      removeEventListener(type,fn){const a=listeners.get(type)||[];listeners.set(type,a.filter(v=>v!==fn));},
      dispatchEvent(event){event.target ||= this;for(const fn of listeners.get(event.type)||[])fn.call(this,event);return !event.defaultPrevented;},
    },properties);
  }
  function element(tag) {
    const attributes=new Map();
    return target(tag,{
      tagName:tag.toUpperCase(),style:{},dataset:{},hidden:false,tabIndex:0,className:'',textContent:'',innerHTML:'',value:'',children:[],parentNode:null,
      width:0,height:0,offsetWidth:dimensions[0],offsetHeight:dimensions[1],scrollTop:0,scrollLeft:0,
      setAttribute(k,v){used.add('element.setAttribute');attributes.set(k,String(v));},
      getAttribute(k){used.add('element.getAttribute');return attributes.get(k)??null;},
      hasAttribute(k){return attributes.has(k);},
      removeAttribute(k){attributes.delete(k);},
      contains(node){for(let n=node;n;n=n.parentNode)if(n===this)return true;return false;},
      blur(){if(document.activeElement===this)document.activeElement=body;},
      appendChild(child){used.add('element.appendChild');child.parentNode=this;this.children.push(child);return child;},
      removeChild(child){this.children=this.children.filter(v=>v!==child);for(const list of classLists){const i=list.indexOf(child);if(i>=0)list.splice(i,1);}child.parentNode=null;return child;},
      remove(){if(this.parentNode)this.parentNode.removeChild(this);},
      focus(){used.add('element.focus');document.activeElement=this;},
      getBoundingClientRect(){return {x:0,y:0,left:0,top:0,width:this.offsetWidth,height:this.offsetHeight};},
      setSelectionRange(a,b){this.selectionStart=a;this.selectionEnd=b;},
    });
  }
  const canvas=element('canvas'),loader=element('div'),note=element('div'),body=element('body');
  loader.appendChild(note);
  loader.className='canvas_loader';
  canvas.className='full_canvas';body.appendChild(canvas);body.appendChild(loader);
  const credits=element('dialog'),creditsLink=element('button');
  credits.showModal=()=>{credits.open=true;};credits.close=()=>{credits.open=false;};
  body.appendChild(credits);body.appendChild(creditsLink);
  const elements={'loader':loader,'loader_note':note,'credits':credits,'credits_link':creditsLink};
  const selectors={'canvas.full_canvas':canvas};
  document=target('document',{
    body,documentElement:element('html'),activeElement:body,fullscreenElement:null,fullscreenEnabled:true,visibilityState:'visible',hidden:false,title:'',
    createElement(tag){used.add(`document.createElement(${tag})`);return element(tag);},
    getElementById(id){used.add(`document.getElementById(${id})`);return elements[id]||null;},
    querySelector(selector){used.add(`document.querySelector(${selector})`);return selectors[selector]||null;},
    getElementsByClassName(name){used.add(`document.getElementsByClassName(${name})`);const list=body.children.filter(e=>e.className.split(' ').includes(name));classLists.push(list);return list;},
    hasFocus(){return true;},
  });
  const navigator={hardwareConcurrency:environment.cores??4,userAgent:'Node packaged page gate',platform:'Linux',maxTouchPoints:0};
  navigator.storage={estimate:async()=>({usage:0,quota:1<<30})};
  const records=new Map();
  const request=result=>{const r={result};queueMicrotask(()=>r.onsuccess?.());return r;};
  const database={
    transaction(){
      used.add('IndexedDB.transaction');const tx={};
      tx.objectStore=()=>({
        get:key=>request(records.get(key)),
        put:value=>{records.set(value.id,value);queueMicrotask(()=>tx.oncomplete?.());},
        delete:key=>{records.delete(key);queueMicrotask(()=>tx.oncomplete?.());},
        index:()=>({openCursor:namespace=>{
          const rows=[...records.values()].filter(v=>v.namespace===namespace).sort((a,b)=>a.id<b.id?-1:a.id>b.id?1:0);
          let i=0;const r={};
          const advance=()=>queueMicrotask(()=>{r.result=i<rows.length?{value:rows[i++],continue:advance}:null;r.onsuccess?.();});
          advance();return r;
        }}),
      });return tx;
    },
  };
  const pathname=environment.path??'/index.html';
  const mount=pathname.slice(0,pathname.lastIndexOf('/')+1),requests=[];
  const location={protocol:'http:',hostname:'localhost',host:'localhost',pathname,search:'',hash:'',href:'http://localhost'+pathname};
  const timers={
    setTimeout:(fn,ms=0,...args)=>clock.timer(fn,ms,false,args),clearTimeout:id=>clock.timers.delete(id),
    setInterval:(fn,ms=0,...args)=>clock.timer(fn,ms,true,args),clearInterval:id=>clock.timers.delete(id),
    requestAnimationFrame:fn=>clock.raf(fn),cancelAnimationFrame:id=>clock.rafs.delete(id),
  };
  const window=target('window',{...timers,navigator,location,scrollX:0,scrollY:0,innerWidth:dimensions[0],innerHeight:dimensions[1],devicePixelRatio:dimensions[2]});
  class VirtualDate extends Date { constructor(...args){super(...(args.length?args:[VirtualDate.now()]));}static now(){return EPOCH*1000+Math.floor(clock.read()*1000);} }
  const context={
    WebAssembly,SharedArrayBuffer,TextDecoder,TextEncoder,Uint8Array,Uint32Array,Float32Array,Float64Array,ArrayBuffer,DataView,
    Blob,URL,URLSearchParams,Response,Request,Headers,DecompressionStream,AbortController,Date:VirtualDate,
    console:Object.fromEntries(['log','warn','error','debug','info'].map(k=>[k,(...args)=>log(k,...args)])),
    performance:{now:()=>clock.read()*1000,timeOrigin:EPOCH*1000},window,document,navigator,location,...timers,queueMicrotask,
    indexedDB:{open(){used.add('IndexedDB.open');return request(database);}},IDBKeyRange:{only:value=>value},
    fetch:async(url,opts={})=>{
      used.add('fetch(local packaged file)');
      const u=new URL(url,location.href);
      requests.push({url:u.href,method:opts.method||'GET'});
      if(u.origin!=='http://localhost')throw new Error(`Network forbidden: ${u.origin}`);
      if(opts.method && !['GET','HEAD'].includes(opts.method))throw new Error(`Unsupported fetch method: ${opts.method}`);
      if(!u.pathname.startsWith(mount))throw new Error(`Fetch outside page mount ${mount}: ${u.pathname}`);
      const file=path.resolve(page,decodeURIComponent(u.pathname.slice(mount.length)));
      if(!file.startsWith(path.resolve(page)+path.sep))throw new Error('Fetch escaped packaged page');
      try {
        const b=fs.readFileSync(file);
        if(opts.method==='HEAD')return new Response(null,{headers:{'content-length':String(b.length)}});
        const range=new Headers(opts.headers).get('range');
        if(range!==null) {
          const match=/^bytes=\s*(\d+)\s*-\s*(\d*)$/.exec(range);
          if(!match)throw new Error(`Unsupported byte range: ${range}`);
          const lo=Math.min(Number(match[1]),b.length),hi=Math.min(match[2]===''?b.length:Number(match[2])+1,b.length);
          if(hi<lo)throw new Error(`Invalid byte range: ${range}`);
          return new Response(b.subarray(lo,hi),{status:206,headers:{'content-length':String(hi-lo),'content-range':`bytes ${lo}-${Math.max(0,hi-1)}/${b.length}`}});
        }
        return new Response(b,{headers:{'content-length':String(b.length)}});
      }
      catch(e){if(e.code==='ENOENT')return new Response(null,{status:404});throw e;}
    },
    ResizeObserver:class {constructor(){used.add('ResizeObserver');}observe(){}unobserve(){}disconnect(){}},
    Worker:class {constructor(){throw new Error('Unshared page attempted a Worker');}},
  };
  const device=audio(clock);
  Object.assign(context,{AudioContext:device.AudioContext,AudioWorkletNode:device.AudioWorkletNode});
  Object.assign(window,{performance:context.performance});
  return {context,window,document,canvas,loader,note,used,requests,audio:device};
}
function event(type,clock,props={}) {
  return {type,timeStamp:clock.now*1000,button:0,charCode:0,repeat:false,shiftKey:false,ctrlKey:false,altKey:false,metaKey:false,
    preventDefault(){this.defaultPrevented=true;},stopPropagation(){},...props};
}
module.exports={Clock,browser,event};
