'use strict';
// Extended from PAGEABI's loader-audit.js: the packaged import object, bridge,
// memory and storage reply serializers remain real. Now boot through index.html,
// bind real DOM input, step the virtual browser and compare every GL frame.
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm'),assert=require('node:assert/strict');
const {Clock,browser}=require('./dom'),{Gl}=require('./gl'),{script,replay}=require('./input');
const {detailsPath,frameAt}=require('./frames');
const settle=()=>new Promise(setImmediate);
async function run(page,oracle,out,scriptPath,probe) {
  const reference=JSON.parse(fs.readFileSync(oracle,'utf8'));
  assert.equal(reference.runs.length,1,'nodegate needs one driver run, not merged coverage');
  if(probe) {
    assert(Number.isSafeInteger(probe.frames)&&probe.frames>0,'probe requires a positive frame count');
    Object.assign(reference.runs[0],{environment:probe.environment,window:probe.window,until:'',deadline:probe.frames/60+10});
  }
  const driver=reference.runs[0],clock=new Clock(driver.environment),logs=[];
  const details=detailsPath(out),fd=fs.openSync(details,'w');
  const gl=new Gl(frame=>fs.writeFileSync(fd,JSON.stringify(frame)+'\n'),driver.environment?.parallel_compile);
  let stopped=false;
  const dom=browser(page,driver.window,clock,(level,...parts)=>{
    const text=parts.map(String).join(' ');logs.push({level,time:clock.now,text});
    if(driver.until&&text.includes(driver.until))stopped=true;
  },driver.environment);
  dom.context.location.search=driver.query;
  const parsed=script(scriptPath?fs.readFileSync(scriptPath,'utf8'):'');
  if(driver.environment?.phone || parsed.inputs.some(input=>input.kind.startsWith('touch-'))) {
    dom.context.navigator.maxTouchPoints=1;
    dom.context.navigator.userAgent='Android Mobile Node packaged page gate';
  }
  const report={kind:probe?'startup-probe':'digest-gate',node:process.version,driver,oracle,frames:gl.frames,frame_details:details,logs,requests:dom.requests,dom:[],compared:0,equal:0};
  report.testClocks=[];
  dom.context.__bt_audio=()=>report.testClocks.push(structuredClone(dom.context.makepad_test_clock));
  const events=process.env.NODEGATE_EVENTS?[]:null;
  if(events)report.events=events;
  let host,loaded,frameCount=0,dispatchReplies=[];
  try {
    assert(!driver.script||scriptPath,'the trace driver requires its input script');
    const html=fs.readFileSync(path.join(page,'index.html'),'utf8');
    const inline=/<script type="module">([\s\S]*?)<\/script>/.exec(html)?.[1];
    assert(inline,'index.html module startup missing');
    const imported=/import\s*\{\s*start_app\s*\}\s*from\s*"(.+)";/.exec(inline);
    assert(imported,'index.html must import its packaged start_app');
    const jsFile=path.resolve(page,imported[1]);assert(jsFile.startsWith(path.resolve(page)+path.sep));
    const js=fs.readFileSync(jsFile,'utf8');
    const context=vm.createContext(dom.context);
    const bridge=vm.runInContext(js.replace(/^export /gm,'')+'\n({WasmBridge,WasmWebBrowser,WasmWebGL,start_app,makepad_crash_reporter})',context,{filename:jsFile});
    const proto=bridge.WasmWebGL.prototype;
    // Keep program reads independent of diagnostic host reads: both advance,
    // but adding a loader log cannot change the program's seeded input stream.
    // The packaged import implementations still call performance.now/Date.now.
    for(const [owner,name] of [[bridge.WasmWebBrowser.prototype,'js_monotonic_now'],[bridge.WasmBridge.prototype,'js_time_now']]) {
      const original=owner[name];
      owner[name]=function(...args){const previous=clock.program;clock.program=true;try{return original.apply(this,args);}finally{clock.program=previous;}};
    }
    const instantiate=bridge.WasmBridge.instantiate_wasm;
    bridge.WasmBridge.instantiate_wasm=async function(...args){
      const wasm=await instantiate.apply(this,args);
      if(driver.stress){assert.equal(typeof wasm.exports.wasm_script_gc_stress,'function');wasm.exports.wasm_script_gc_stress(1);}
      return wasm;
    };
    // Replace only the GL side at the message boundary, not the loader's
    // startup, env imports, serialization, pumping, events, timers or RAF.
    proto.init_webgl_context=function(){
      host=this;assert(driver.environment?.gpu_info,'trace lacks browser environment');this.gpu_info={...driver.environment.gpu_info};
      return true;
    };
    const load=proto.load_deps;
    proto.load_deps=function(){loaded=load.call(this);loaded.catch(error=>startReject(error));return loaded;};
    for(const name of ['gpu_timer_poll','gpu_timer_begin','gpu_timer_end','gpu_watchdog_submit'])proto[name]=()=>{};
    proto.gpu_watchdog_hold=()=>false;
    proto.poll_pending_webgl_shaders=function(){return gl.poll(this);};
    for(const name of Object.getOwnPropertyNames(proto))if(name.startsWith('FromWasm')) {
      const original=proto[name];
      proto[name]=function(m){if(!gl.handle(name,m,this,dispatchReplies))return original.call(this,m);};
    }
    // Bridge dispatch is synchronous. GL readbacks queue at the end of its
    // batch just as page.rs dispatch does, after all pointers were consumed.
    const from=bridge.WasmBridge.prototype.new_from_wasm;
    bridge.WasmBridge.prototype.new_from_wasm=function(...args){
      const message=from.apply(this,args),dispatch=message.dispatch_on_app;
      message.dispatch_on_app=function(){
        const outer=dispatchReplies;dispatchReplies=[];dispatch.call(this);
        const replies=dispatchReplies;dispatchReplies=outer;for(const f of replies)f();
      };return message;
    };
    const to=bridge.WasmBridge.prototype.new_to_wasm;
    bridge.WasmBridge.prototype.new_to_wasm=function(...args){
      const message=to.apply(this,args),animation=message.ToWasmAnimationFrame;
      if(events)for(const name of Object.getOwnPropertyNames(Object.getPrototypeOf(message)))if(name.startsWith('ToWasm')) {
        const fn=message[name];message[name]=function(...values){events.push({time:clock.now,kind:name,args:values});return fn.apply(this,values);};
      }
      const frame=message.ToWasmAnimationFrame;
      if(animation)message.ToWasmAnimationFrame=function(...values){frameCount++;return frame.apply(this,values);};
      return message;
    };
    context.__page=bridge;
    let startResolve,startReject;
    const startup=new Promise((resolve,reject)=>{startResolve=resolve;startReject=reject;});
    context.__page={...bridge,start_app:async(...args)=>{try{const app=await bridge.start_app(...args);startResolve(app);return app;}catch(error){startReject(error);throw error;}}};
    Object.defineProperty(dom.note,'textContent',{set(value){if(dom.loader.className==='error')startReject(new Error(value));}});
    vm.runInContext(inline.replace(imported[0],'const {start_app}=__page;'),context,{filename:path.join(page,'index.html')});
    await startup;await loaded;await settle();
    assert(host,'packaged start_app did not construct WasmWebGL');
    assert.equal(host.wasm._has_thread_support,false);
    assert(!(host.memory.buffer instanceof SharedArrayBuffer));
    report.imports=WebAssembly.Module.imports(host.wasm._module);
    const end=driver.deadline;assert(Number.isFinite(end),'trace is missing its exact deadline');
    let nextInput=0,lastFrame=0,shift=0;
    const seen=text=>logs.some(line=>line.text.includes(text));
    while(clock.now<end+shift&&!stopped&&(!probe||gl.frames.length<probe.frames)) {
      const input=parsed.inputs[nextInput];
      const blocked=input?.kind==='wait'&&input.time+shift<=clock.now&&!seen(input.args);
      const endAt=end+shift;
      const t=Math.max(clock.now,Math.min(endAt,clock.next(),blocked?clock.now+1/60:(input?.time??Infinity)+shift));
      if(t>=endAt&&!blocked){clock.now=endAt;break;}
      clock.now=t;
      if(blocked){shift=Math.max(shift,t-input.time);assert(shift<=900,'script wait never completed: '+input.args);}
      while(parsed.inputs[nextInput]?.time+shift<=t){
        const input=parsed.inputs[nextInput];
        if(input.kind==='wait'&&!seen(input.args))break;
        nextInput++;replay(input,dom,clock,host);await settle();
      }
      await clock.timersDue(settle,host.poll_timer);
      await clock.framesDue(()=>{if(frameCount!==lastFrame){gl.end(clock.now);lastFrame=frameCount;}},settle);
      await settle();
      if(bridge.makepad_crash_reporter.is_wasm_dead())throw new Error('Packaged loader marked wasm dead');
    }
    assert(!logs.some(s=>s.level==='error'||s.text.includes('__MAKEPAD_WASM_PANIC__')||s.text.startsWith('[E]')),'page reported errors');
    if(probe) {
      assert.equal(gl.frames.length,probe.frames,'page did not reach the requested frame count');
      report.status='PASS';
    } else {
    const expected=reference.frames;
    assert(expected.length>0,'oracle has no frames');
    for(let i=0;i<Math.max(expected.length,gl.frames.length);i++) {
      const a=expected[i],b=gl.frames[i];report.compared++;
      const fields=['time','draw_calls','passes','upload_bytes','digest'];
      if(a&&b&&fields.every(f=>a[f]===b[f])){report.equal++;continue;}
      if(report.firstDifference)continue;
      const mismatch={frame:i+1,field:fields.find(f=>a?.[f]!==b?.[f])||'frame_count',trace:a&&{...a,messages:undefined},node:b&&{...b,messages:undefined}};
      const am=a?frameAt(detailsPath(oracle),i).messages:[],bm=b?frameAt(details,i).messages:[];
      outer:for(let j=0;j<Math.max(am.length,bm.length);j++) {
        if(am[j]?.kind!==bm[j]?.kind){mismatch.message={index:j,kind:bm[j]?.kind,expected:am[j]?.kind,field:'kind'};break;}
        for(let k=0;k<Math.max(am[j].fields.length,bm[j].fields.length);k++) {
          if(am[j].fields[k]!==bm[j].fields[k]?.value){mismatch.message={index:j,kind:am[j].kind,field:bm[j].fields[k]?.field||`fold[${k}]`,trace:am[j].fields[k],node:bm[j].fields[k]?.value};break outer;}
        }
      }
      report.firstDifference=mismatch;
    }
    report.status=report.equal===report.compared?'PASS':'FAIL';
    }
  }catch(error){report.status='FAIL';report.error=String(error);report.stack=error.stack;}
  finally{fs.closeSync(fd);}
  report.audio=dom.audio.summary();
  report.dom=[...dom.used].sort();return report;
}
if(require.main===module) {
  let completed=false;
  process.on('beforeExit',()=>{if(!completed){console.error('nodegate: page startup did not complete');process.exitCode=1;}});
  const args=process.argv.slice(2),probeAt=args.indexOf('--probe');
  let probe;
  if(probeAt>=0){assert.equal(probeAt+2,args.length,'--probe <environment.json> must be last');probe=JSON.parse(fs.readFileSync(args[probeAt+1],'utf8'));args.splice(probeAt);}
  const [page,oracle,out,scriptPath]=args;
  if(!page||!oracle||!out)throw new Error('usage: node loader-audit.js <page-dir> <trace.json> <out.json> [script]');
  run(page,oracle,out,scriptPath,probe).then(report=>{
    completed=true;
    fs.writeFileSync(out,JSON.stringify(report,null,2)+'\n');
    console.log(JSON.stringify({kind:report.kind,status:report.status,frames:report.frames.length,compared:report.compared,equal:report.equal,firstDifference:report.firstDifference,error:report.error,stack:report.stack}));
    if(report.status!=='PASS')process.exitCode=1;
  }).catch(e=>{completed=true;console.error(e);process.exitCode=1;});
}
module.exports={run};
