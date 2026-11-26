'use strict';
// Web Audio's device boundary only. The packaged loader constructs and pumps
// its real FIFO, and its blob module supplies the actual worklet processor.
const vm=require('node:vm'),{resolveObjectURL}=require('node:buffer');
function audio(clock) {
  const devices=[];
  class AudioContext {
    constructor() {
      this.sampleRate=48000;this.state='running';this.start=clock.now;
      this.quanta=0;this.nodes=[];this.destination={};this.baseLatency=0;this.outputLatency=0;
      this.scope=vm.createContext({Float32Array,currentFrame:0,
        AudioWorkletProcessor:class {constructor(){this.port={onmessage:null,postMessage:()=>{}};}},
        registerProcessor:(name,processor)=>{this.processors.set(name,processor);}});
      this.processors=new Map();
      this.audioWorklet={addModule:async url=>{
        const blob=resolveObjectURL(url);if(!blob)throw new Error('Audio module must be the packaged blob');
        vm.runInContext(await blob.text(),this.scope,{filename:'packaged-audio-worklet.js'});
      }};
      devices.push(this);
    }
    advance() {
      if(this.state!=='running')return;
      while(this.start+this.quanta*128/this.sampleRate<=clock.now) {
        this.scope.currentFrame=this.quanta*128;
        for(const node of this.nodes)if(node.connected) {
          const left=new Float32Array(128),right=new Float32Array(128);
          node.processor.process([],[[left,right]]);
          node.blocks++;
          if(left.some(v=>Math.abs(v)>1e-3)||right.some(v=>Math.abs(v)>1e-3))node.audibleBlocks++;
        }
        this.quanta++;
      }
    }
    get currentTime(){this.advance();return this.quanta*128/this.sampleRate;}
    getOutputTimestamp(){const time=this.currentTime;return {contextTime:time,performanceTime:(this.start+time)*1000};}
    resume(){this.state='running';return Promise.resolve();}
    close(){this.state='closed';return Promise.resolve();}
  }
  class AudioWorkletNode {
    constructor(context,name) {
      const Processor=context.processors.get(name);if(!Processor)throw new Error('Unknown audio processor '+name);
      this.processor=new Processor();this.context=context;this.connected=false;this.blocks=0;this.audibleBlocks=0;
      this.port={onmessage:null,postMessage:message=>{context.advance();this.processor.port.onmessage({data:message});}};
      this.processor.port.postMessage=message=>this.port.onmessage?.({data:message});
      context.nodes.push(this);
    }
    connect(){this.connected=true;}
    disconnect(){this.connected=false;}
  }
  return {AudioContext,AudioWorkletNode,summary:()=>devices.map(d=>({sampleRate:d.sampleRate,
    quanta:d.quanta,outputs:d.nodes.map(n=>({blocks:n.blocks,audibleBlocks:n.audibleBlocks,
      consumed:n.processor.consumed,underruns:n.processor.underruns,underrun_frames:n.processor.underrun_frames}))}))};
}
module.exports={audio};
