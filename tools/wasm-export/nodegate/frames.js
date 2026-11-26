'use strict';
// Full ordered field fingerprints live beside the compact frame summaries.
// Stream them while drawing; read at most one frame when explaining a mismatch.
const fs=require('node:fs'),path=require('node:path'),{StringDecoder}=require('node:string_decoder');
function detailsPath(file){const p=path.parse(file);return path.join(p.dir,p.name+'.frames.jsonl');}
function frameAt(file,index) {
  const fd=fs.openSync(file,'r'),buffer=Buffer.alloc(64*1024),decoder=new StringDecoder('utf8');
  let text='',line=0;
  try {
    for(;;) {
      const count=fs.readSync(fd,buffer,0,buffer.length,null);
      text+=count?decoder.write(buffer.subarray(0,count)):decoder.end();
      let end;
      while((end=text.indexOf('\n'))>=0) {
        const value=text.slice(0,end);text=text.slice(end+1);
        if(line++===index)return JSON.parse(value);
      }
      if(!count)break;
    }
    throw new Error(`Frame ${index+1} is absent from ${file}`);
  }finally{fs.closeSync(fd);}
}
module.exports={detailsPath,frameAt};
