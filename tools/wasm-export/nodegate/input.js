'use strict';
// trace/src/lib.rs parse_script syntax. Each action uses the loader's DOM
// listeners; down/move/up retain their time and order for drag replay.
const {event}=require('./dom');
function script(source) {
  const inputs=[];let end;
  const add=(time,kind,args)=>inputs.push({time,kind,args});
  for(const raw of source.split('\n')) {
    const line=raw.split('#')[0].trim();if(!line)continue;
    if(line.startsWith('end ')){end=Number(line.slice(4))/1000;continue;}
    const m=/^at ([^ ]+) ([^ ]+)(?: (.*))?$/.exec(line);if(!m)throw new Error(`Invalid script: ${line}`);
    const time=Number(m[1])/1000,kind=m[2],rest=m[3]||'';
    if(kind==='click'){add(time,'move',rest);add(time,'down',rest);add(time+0.05,'up',rest);}
    else if(kind==='key'){add(time,'keydown',rest);add(time+0.03,'keyup',rest);}
    else if(['down','up','move','touch-down','touch-move','touch-up','scroll','text','resize','wait','gesture'].includes(kind))add(time,kind,rest);
    else throw new Error(`Unknown script action: ${kind}`);
    if(!Number.isFinite(time)||time<0)throw new Error(`Invalid script time: ${line}`);
  }
  inputs.sort((a,b)=>a.time-b.time);return {inputs,end};
}
function replay(input,dom,clock,host) {
  const {kind,args}=input,n=args.split(/\s+/).map(Number),fire=(target,type,props)=>target.dispatchEvent(event(type,clock,props));
  if(kind==='wait')return;
  if(kind==='gesture'){fire(dom.document,'click',{});return;}
  // The real loader deliberately omits mobile keyboard listeners. Do not
  // manufacture a textarea or inject bridge messages around that decision.
  if(['text','keydown','keyup'].includes(kind) && !host.text_area && (host.detect.is_android||host.detect.is_mobile_safari))return;
  const point={pageX:n[0],pageY:n[1],clientX:n[0],clientY:n[1]};
  if(kind.startsWith('touch-')) {
    const touch={...point,identifier:1,radiusX:1,radiusY:1,rotationAngle:0,force:1};
    fire(dom.canvas,{'touch-down':'touchstart','touch-move':'touchmove','touch-up':'touchend'}[kind],{touches:kind==='touch-up'?[]:[touch],changedTouches:[touch]});return;
  }
  const pointer={down:['canvas','mousedown'],up:['window','mouseup'],move:['window','mousemove'],scroll:['canvas','wheel']};
  if(pointer[kind]) {const [target,type]=pointer[kind];fire(dom[target],type,{...point,deltaX:n[2]||0,deltaY:n[3]||0,deltaMode:0});return;}
  if(kind==='text') {
    // A completed IME composition sends the script's complete string in one
    // TextInput (was_paste=false, replace_last=false), including multi-codepoint text.
    fire(host.text_area,'compositionstart',{});fire(host.text_area,'compositionend',{data:args});return;
  }
  if(kind==='keydown'||kind==='keyup') {
    const codes={Return:13,Enter:13,Escape:27,Backspace:8,Tab:9,Space:32,Home:36,Delete:46,ArrowLeft:37,ArrowUp:38,ArrowRight:39,ArrowDown:40};
    const keyCode=codes[args]??Number(args);if(!Number.isFinite(keyCode))throw new Error(`Invalid key: ${args}`);
    fire(host.text_area,kind,{keyCode});return;
  }
  if(kind==='resize') {
    const [w,h,dpi]=args.split(/[x@]/).map(Number);
    Object.assign(dom.window,{innerWidth:w,innerHeight:h,devicePixelRatio:dpi||1});
    Object.assign(dom.canvas,{offsetWidth:w,offsetHeight:h});fire(dom.window,'resize',{});return;
  }
  throw new Error(`Unhandled input: ${kind}`);
}
module.exports={script,replay};
