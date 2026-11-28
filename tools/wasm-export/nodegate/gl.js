'use strict';
// Byte-for-byte port of trace/src/gl.rs. Field labels explain a differing fold;
// they are metadata, never part of the digest. No pointer value is hashed.
const hex = n => n.toString(16).padStart(16, '0');
class Digest {
  constructor() { this.hi=0xcbf29ce4; this.lo=0x84222325; this.fields = []; }
  get value() { return (BigInt(this.hi)<<32n)|BigInt(this.lo); }
  bytes(bytes, field) {
    let hi=this.hi,lo=this.lo,fh=0xcbf29ce4,fl=0x84222325;
    // FNV's prime is 2^40 + 435. Two 32-bit limbs give exactly the
    // same wrapping u64 multiply, without allocating BigInts per byte.
    // Each low product is below 2^41, hence exact in a JS number.
    for (const b of bytes) {
      lo=(lo^b)>>>0;const p=lo*435;
      hi=(Math.imul(hi,435)+Math.floor(p/4294967296)+(lo<<8))>>>0;lo=p>>>0;
      fl=(fl^b)>>>0;const fp=fl*435;
      fh=(Math.imul(fh,435)+Math.floor(fp/4294967296)+(fl<<8))>>>0;fl=fp>>>0;
    }
    this.hi=hi;this.lo=lo;
    this.fields.push({field, value: `${bytes.length}:${hex((BigInt(fh)<<32n)|BigInt(fl))}`});
  }
  u64(value, field) {
    const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt.asUintN(64, BigInt(value)));
    this.bytes(b, field);
  }
  str(value, field) {
    const b = Buffer.from(value); this.u64(b.length, `${field}.length`); this.bytes(b, field);
  }
}
const f32 = value => { const b = Buffer.alloc(4); b.writeFloatLE(value); return b.readUInt32LE(); };
class Gl {
  constructor(record,parallel=false) { this.record=record; this.parallel=parallel; this.pending=new Set(); this.frames = []; this.waiting = new Set(); this.spread = false; this.reset(); }
  poll(host) {
    const count=this.pending.size;
    for(const id of [...this.pending].sort((a,b)=>a-b)) {
      if(this.spread){this.waiting.add(id);host.to_wasm.ToWasmWebGLShadersLinked({count:1});}
      else host.to_wasm.ToWasmWebGLShadersDone({count:1,waited:0});
    }
    this.pending.clear();host.pending_webgl_shader_count=0;
    return count;
  }
  reset() { this.hash = new Digest(); this.frame = {draw_calls:0, passes:0, upload_bytes:0, messages:[]}; }
  end(time) {
    const frame={...this.frame,time,digest:hex(this.hash.value)};
    this.record(frame);
    this.frames.push({...frame,messages:undefined}); this.reset();
  }
  handle(kind, m, host, replies) {
    const h = this.hash, u = (v, f) => h.u64(v, f);
    const fields = (o, names, prefix = '') => names.split(' ').forEach(k => u(o[k], prefix+k));
    const bytes = (p, scale) => new Uint8Array(host.memory.buffer, p.ptr, p.len * scale);
    const upload = (tag, id, b, field='data') => {
      u(tag,'kind'); u(id,'id'); u(b.length,`${field}.length`); h.bytes(b,field); this.frame.upload_bytes += b.length;
    };
    const reply = (name, args, pump=false) => replies.push(() => {host.to_wasm[name](args); if(pump)host.do_wasm_pump();});
    const done = (count, waited) => reply('ToWasmWebGLShadersDone',{count,waited});
    const colors = (c,p='clear_color') => ['r','g','b','a'].forEach(k=>u(f32(c[k]),`${p}.${k}`));
    switch(kind) {
      case 'FromWasmSpreadFirstDraws':
        this.spread=m.on;
        if(!m.on && this.waiting.size) {
          done(this.waiting.size,this.waiting.size); this.waiting.clear();
          host.to_wasm.ToWasmRedrawAll({});host.FromWasmRequestAnimationFrame();
        }
        break;
      case 'FromWasmCompileWebGLShader':
        u(1,'kind');u(m.shader_id,'shader_id');h.str(m.vertex,'vertex');h.str(m.pixel,'pixel');
        if(this.parallel){this.pending.add(m.shader_id);host.pending_webgl_shader_count=this.pending.size;host.FromWasmRequestAnimationFrame();break;}
        if(this.spread) {this.waiting.add(m.shader_id);reply('ToWasmWebGLShadersLinked',{count:1});} else done(1,0);
        break;
      case 'FromWasmAllocArrayBuffer':
      case 'FromWasmAllocIndexBuffer': {
        const index=kind==='FromWasmAllocIndexBuffer';
        if(index)u(m.index_width,'index_width');
        const b=Buffer.concat([Buffer.from(bytes(m.data,4)),Buffer.from(bytes(m.byte_data,1))]);
        upload(index?6:2,m.buffer_id,b);break;
      }
      case 'FromWasmCreateInstanceSlab':
        u(3,'kind');fields(m,'slab_slot backing_generation capacity_bytes');break;
      case 'FromWasmWriteInstanceRegion':
        u(4,'kind');fields(m,'backing_generation');fields(m.region,'index gen','region.');fields(m,'byte_off');
        upload(5,m.slab_slot,Buffer.concat(m.parts.map(p=>Buffer.from(bytes(p,1)))),'parts');break;
      case 'FromWasmAllocVao':
        u(7,'kind');fields(m,'vao_id shader_id geom_ib_id geom_vb_id slab_slot backing_generation');
        fields(m.region,'index gen','region.');fields(m,'byte_off byte_len instance_byte_offset');break;
      case 'FromWasmFreeWebGLResources':
        u(8,'kind');for(const key of ['array_buffer_ids','index_buffer_ids','vao_ids','texture_ids','framebuffer_ids']) {
          u(m[key].length,`${key}.length`);m[key].forEach((v,i)=>u(v,`${key}[${i}]`));
        }
        u(m.instance_slabs.length,'instance_slabs.length');
        m.instance_slabs.forEach((s,i)=>fields(s,'slab_slot backing_generation',`instance_slabs[${i}].`));break;
      case 'FromWasmAllocTextureImage2D_BGRAu8_32':
      case 'FromWasmAllocTextureImage2D_Ru8':
      case 'FromWasmAllocTextureImage2D_RGBAf32':
      case 'FromWasmAllocTextureCube_BGRAu8_32': {
        const tags={FromWasmAllocTextureImage2D_BGRAu8_32:9,FromWasmAllocTextureImage2D_Ru8:10,
          FromWasmAllocTextureImage2D_RGBAf32:11,FromWasmAllocTextureCube_BGRAu8_32:12};
        fields(m,'width height');upload(tags[kind],m.texture_id,bytes(m.data,tags[kind]===10?1:4));break;
      }
      case 'FromWasmBeginRenderTexture':
        this.frame.passes++;u(13,'kind');fields(m,'pass_id width height');
        m.color_targets.forEach((c,i)=>{fields(c,'texture_id init_only format',`color_targets[${i}].`);colors(c.clear_color,`color_targets[${i}].clear_color`);});
        fields(m.depth_target,'attached texture_id','depth_target.');u(f32(m.depth_target.clear_depth),'depth_target.clear_depth');break;
      case 'FromWasmBeginRenderCanvas':
        this.frame.passes++;u(14,'kind');colors(m.clear_color);u(f32(m.clear_depth),'clear_depth');break;
      case 'FromWasmUploadUniformStream':
        this.uniform_stream=Buffer.from(bytes(m.data,1));break;
      case 'FromWasmDrawCall':
        if(this.pending.has(m.shader_id))break;
        if(this.waiting.has(m.shader_id)) {
          if(host.first_draw_this_frame) {host.first_draws_deferred=true;break;}
          host.first_draw_this_frame=true;this.waiting.delete(m.shader_id);done(1,1);
        }
        this.frame.draw_calls++;u(15,'kind');
        fields(m,'vao_id shader_id index_width depth_write alpha_blend blend_max cull_mode');
        // each block's bytes in the repaint's uniform stream
        for(const key of ['pass_uniforms','draw_list_uniforms','draw_call_uniforms','user_uniforms','live_uniforms']) {
          const s=this.uniform_stream||Buffer.alloc(0),r=m[key];
          const b=s.subarray(Math.min(r.off,s.length),Math.min(r.off+r.len,s.length));u(b.length,`${key}.length`);h.bytes(b,key);
        }
        m.custom_uniforms.forEach((c,i)=>{h.str(c.block_name,`custom_uniforms[${i}].block_name`);h.bytes(bytes(c.data,1),`custom_uniforms[${i}].data`);});
        m.textures.forEach((v,i)=>u(v==null?0xffffffffffffffffn:v,`textures[${i}]`));break;
      case 'FromWasmPollGpuCompletion': reply('ToWasmGpuCompletion',{...m,success:true},true);break;
      case 'FromWasmRequestRenderTextureCapture':
        if(!m.register_only)reply('ToWasmRenderTextureCapture',{...m,offset:0,complete:true,data:new Uint8Array(m.width*m.height*4),error:''});break;
      case 'FromWasmSetDefaultDepthAndBlendMode':break;
      default:return false;
    }
    if(h.fields.length) {
      this.frame.messages.push({kind, fields:h.fields});h.fields=[];
    }
    return true;
  }
}
module.exports={Digest,Gl,hex};
if(require.main===module) {
  const d=new Digest();d.u64(0n);d.u64(0xffffffffffffffffn);d.u64(0x0123456789abcdefn);
  d.str('GL π\0😀');d.bytes(Uint8Array.from([0,1,127,128,255]));d.bytes(new Uint8Array());
  console.log(hex(d.value));
}
