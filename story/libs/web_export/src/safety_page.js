// The browser safety gate's probe (makepad-story-web-export, safety.rs).
// Installed into the page before any of its scripts run; the gate reads
// `window.__mp_safety_sample()` once a second. Nothing here changes what the
// page does: it wraps WebGL2 and WebAssembly calls only to count.
//
// - wasm memory: every WebAssembly.Memory the page makes or a module
//   exports, summed (buffer.byteLength), and its peak;
// - GPU memory: the bytes the page has allocated through WebGL2 (buffers,
//   textures by level and face, renderbuffers with their samples), less what
//   it deleted, plus the canvas's drawing buffers; and its peak; and the
//   live WebGL objects (created less deleted) by kind, for leaks; and a
//   breakdown of what holds it (`__mp_safety_breakdown()`);
// - draw calls, and WebAssembly.Memory.grow calls;
// - the runtime's own `makepad_gpu_stats()` (watchdog and GPU ledger)
//   where it publishes it: the gate prefers it to the probe's numbers;
// - context losses (webglcontextlost on any WebGL canvas);
// - frame GPU time: the results of the page's own TIME_ELAPSED queries
//   (the runtime's frame timer); where the page makes none, the probe times
//   each animation frame itself; where the extension is missing, nothing,
//   and the gate falls back to frame intervals;
// - frame intervals (requestAnimationFrame to requestAnimationFrame);
// - the media elements (the song): position, paused, ended.
// The runtime's GL error queries (getError, framebuffer status) run only
// when the page asks for them before it loads: on for the gate's run, off
// in the exported film.
window.makepad_gl_checks = true;

(() => {
    const S = {
        wasm: 0, wasm_peak: 0, gpu: 0, gpu_peak: 0, lost: 0, restored: 0,
        frames: 0, interval_max: 0, gpu_ms_max: 0, gpu_samples: 0, gpu_source: "none",
        disjoint: false, own_queries: false, foreign_queries: 0, errors: [],
    };
    window.__mp_safety = S;
    const memories = [];
    const media = [];
    const add_memory = m => { if (m instanceof WebAssembly.Memory && !memories.includes(m)) memories.push(m); };
    const scan_exports = inst => {
        try { for (const v of Object.values(inst.exports)) add_memory(v); } catch (e) {}
    };
    // Memory.grow: when and by how much (the gate fails a grow after the
    // film's first plate).
    S.grows = 0;
    const grow = WebAssembly.Memory.prototype.grow;
    WebAssembly.Memory.prototype.grow = function (pages) { S.grows++; return grow.call(this, pages); };
    const NativeMemory = WebAssembly.Memory;
    WebAssembly.Memory = function (desc) { const m = new NativeMemory(desc); add_memory(m); return m; };
    WebAssembly.Memory.prototype = NativeMemory.prototype;
    const instantiate = WebAssembly.instantiate;
    WebAssembly.instantiate = function (...a) {
        return instantiate.apply(this, a).then(r => { scan_exports(r.instance || r); return r; });
    };
    if (WebAssembly.instantiateStreaming) {
        const streaming = WebAssembly.instantiateStreaming;
        WebAssembly.instantiateStreaming = function (...a) {
            return streaming.apply(this, a).then(r => { scan_exports(r.instance); return r; });
        };
    }
    const NativeInstance = WebAssembly.Instance;
    WebAssembly.Instance = function (module, imports) { const i = new NativeInstance(module, imports); scan_exports(i); return i; };
    WebAssembly.Instance.prototype = NativeInstance.prototype;

    const create = Document.prototype.createElement;
    Document.prototype.createElement = function (name, ...rest) {
        const el = create.call(this, name, ...rest);
        if (el instanceof HTMLMediaElement) media.push(el);
        return el;
    };

    // GPU bytes per object: buffers and renderbuffers by size, textures by
    // (face, level).
    const P = WebGL2RenderingContext.prototype;
    const sizes = new WeakMap();
    const contexts = [];
    const state = gl => {
        let s = gl.__mp_safety;
        if (!s) {
            s = gl.__mp_safety = { buffers: new Map(), textures: new Map(), unit: 0, renderbuffer: null };
            contexts.push(gl);
        }
        return s;
    };
    // What each live texture and renderbuffer is (format, size), for the
    // breakdown at the peak.
    const meta = new Map();
    const describe = (gl, obj, kind, internal, w, h, depth) => { if (obj) meta.set(obj, { kind, internal, w, h, depth: depth || 1 }); };
    const set_bytes = (obj, key, bytes) => {
        if (!obj) return;
        let m = sizes.get(obj);
        if (!m) { m = new Map(); sizes.set(obj, m); }
        S.gpu += bytes - (m.get(key) || 0);
        m.set(key, bytes);
        if (S.gpu > S.gpu_peak) S.gpu_peak = S.gpu;
    };
    const drop = obj => {
        const m = obj && sizes.get(obj);
        if (!m) return;
        for (const b of m.values()) S.gpu -= b;
        sizes.delete(obj);
        meta.delete(obj);
    };
    const bpp = (gl, internal, type) => {
        const t = {
            [gl.R8]: 1, [gl.R8UI]: 1, [gl.R8I]: 1, [gl.ALPHA]: 1, [gl.LUMINANCE]: 1, [gl.R16F]: 2, [gl.RG8]: 2, [gl.R16UI]: 2, [gl.R16I]: 2,
            [gl.LUMINANCE_ALPHA]: 2, [gl.DEPTH_COMPONENT16]: 2, [gl.RGB565]: 2, [gl.RGBA4]: 2, [gl.RGB5_A1]: 2,
            [gl.R32F]: 4, [gl.R32UI]: 4, [gl.R32I]: 4, [gl.RG16F]: 4, [gl.RGBA8]: 4, [gl.SRGB8_ALPHA8]: 4, [gl.RGB8]: 4, [gl.SRGB8]: 4,
            [gl.RGB10_A2]: 4, [gl.R11F_G11F_B10F]: 4, [gl.RGB9_E5]: 4, [gl.DEPTH_COMPONENT24]: 4, [gl.DEPTH24_STENCIL8]: 4,
            [gl.DEPTH_COMPONENT32F]: 4, [gl.RGBA8UI]: 4, [gl.RGBA8I]: 4,
            [gl.RG32F]: 8, [gl.RGBA16F]: 8, [gl.RGB16F]: 8, [gl.DEPTH32F_STENCIL8]: 8, [gl.RG32UI]: 8, [gl.RGBA16UI]: 8,
            [gl.RGBA32F]: 16, [gl.RGB32F]: 16, [gl.RGBA32UI]: 16, [gl.RGBA32I]: 16,
        }[internal];
        if (t) return t;
        // Unsized formats: by the data type.
        if (type === gl.FLOAT) return 16;
        if (type === gl.HALF_FLOAT) return 8;
        return 4;
    };
    const face_key = (gl, target, level) => `${target === gl.TEXTURE_2D || target === gl.TEXTURE_3D || target === gl.TEXTURE_2D_ARRAY ? 0 : target}:${level}`;
    const bound_texture = (gl, target) => {
        const s = state(gl);
        const t = target >= gl.TEXTURE_CUBE_MAP_POSITIVE_X && target <= gl.TEXTURE_CUBE_MAP_NEGATIVE_Z ? gl.TEXTURE_CUBE_MAP : target;
        return s.textures.get(`${s.unit}:${t}`);
    };
    const wrap = (name, after) => {
        const f = P[name];
        if (!f) return;
        P[name] = function (...a) {
            const r = f.apply(this, a);
            try { after(this, a, r); } catch (e) { if (S.errors.length < 8) S.errors.push(`${name}: ${e}`); }
            return r;
        };
    };
    // Live objects: created less deleted (a leak shows as a count that
    // keeps rising).
    S.live = { textures: 0, buffers: 0, renderbuffers: 0, framebuffers: 0 };
    for (const [make, del, key] of [["createTexture", "deleteTexture", "textures"], ["createBuffer", "deleteBuffer", "buffers"], ["createRenderbuffer", "deleteRenderbuffer", "renderbuffers"], ["createFramebuffer", "deleteFramebuffer", "framebuffers"]]) {
        const made = new WeakSet();
        const f = P[make], g = P[del];
        P[make] = function (...a) { const o = f.apply(this, a); if (o) { made.add(o); S.live[key]++; } return o; };
        P[del] = function (o, ...a) { if (o && made.has(o)) { made.delete(o); S.live[key]--; } return g.call(this, o, ...a); };
    }
    wrap("bindBuffer", (gl, [target, buf]) => state(gl).buffers.set(target, buf));
    wrap("bufferData", (gl, [target, src, usage, offset, length]) => {
        let bytes = 0;
        if (typeof src === "number") bytes = src;
        else if (src) {
            const el = src.BYTES_PER_ELEMENT || 1;
            bytes = length ? length * el : src.byteLength - (offset || 0) * el;
        }
        set_bytes(state(gl).buffers.get(target), "data", bytes);
    });
    wrap("deleteBuffer", (gl, [buf]) => drop(buf));
    wrap("activeTexture", (gl, [unit]) => { state(gl).unit = unit - gl.TEXTURE0; });
    wrap("bindTexture", (gl, [target, tex]) => { const s = state(gl); s.textures.set(`${s.unit}:${target}`, tex); });
    wrap("texStorage2D", (gl, [target, levels, internal, w, h]) => {
        const faces = target === gl.TEXTURE_CUBE_MAP ? 6 : 1;
        let bytes = 0;
        for (let l = 0; l < levels; l++) bytes += Math.max(1, w >> l) * Math.max(1, h >> l) * bpp(gl, internal) * faces;
        set_bytes(bound_texture(gl, target), "storage", bytes);
        describe(gl, bound_texture(gl, target), target === gl.TEXTURE_CUBE_MAP ? "cube" : "texture", internal, w, h);
    });
    wrap("texStorage3D", (gl, [target, levels, internal, w, h, d]) => {
        let bytes = 0;
        for (let l = 0; l < levels; l++) bytes += Math.max(1, w >> l) * Math.max(1, h >> l) * (target === gl.TEXTURE_3D ? Math.max(1, d >> l) : d) * bpp(gl, internal);
        set_bytes(bound_texture(gl, target), "storage", bytes);
        describe(gl, bound_texture(gl, target), "texture3d", internal, w, h, d);
    });
    wrap("texImage2D", (gl, a) => {
        const [target, level, internal] = a;
        let w, h, type;
        if (a.length >= 8) { w = a[3]; h = a[4]; type = a[7]; }
        else { const src = a[5]; type = a[4]; w = src.videoWidth || src.naturalWidth || src.displayWidth || src.width || 0; h = src.videoHeight || src.naturalHeight || src.displayHeight || src.height || 0; }
        set_bytes(bound_texture(gl, target), face_key(gl, target, level), w * h * bpp(gl, internal, type));
        if (level === 0) describe(gl, bound_texture(gl, target), "texture", internal, w, h);
    });
    wrap("texImage3D", (gl, [target, level, internal, w, h, d, border, format, type]) => {
        set_bytes(bound_texture(gl, target), face_key(gl, target, level), w * h * d * bpp(gl, internal, type));
    });
    wrap("compressedTexImage2D", (gl, [target, level, internal, w, h, border, src]) => {
        set_bytes(bound_texture(gl, target), face_key(gl, target, level), src && src.byteLength ? src.byteLength : w * h);
    });
    wrap("copyTexImage2D", (gl, [target, level, internal, x, y, w, h]) => {
        set_bytes(bound_texture(gl, target), face_key(gl, target, level), w * h * bpp(gl, internal));
    });
    wrap("deleteTexture", (gl, [tex]) => drop(tex));
    wrap("bindRenderbuffer", (gl, [target, rb]) => { state(gl).renderbuffer = rb; });
    wrap("renderbufferStorage", (gl, [target, internal, w, h]) => { set_bytes(state(gl).renderbuffer, "storage", w * h * bpp(gl, internal)); describe(gl, state(gl).renderbuffer, "renderbuffer", internal, w, h); });
    wrap("renderbufferStorageMultisample", (gl, [target, samples, internal, w, h]) => { set_bytes(state(gl).renderbuffer, "storage", w * h * bpp(gl, internal) * Math.max(1, samples)); describe(gl, state(gl).renderbuffer, `renderbuffer x${samples}`, internal, w, h); });
    wrap("deleteRenderbuffer", (gl, [rb]) => drop(rb));

    // Draw calls (the gate fails a second of play with none).
    S.draws = 0;
    for (const name of ["drawArrays", "drawElements", "drawArraysInstanced", "drawElementsInstanced", "drawRangeElements", "drawBuffers"]) {
        if (name === "drawBuffers") continue;
        const f = P[name];
        if (f) P[name] = function (...a) { S.draws++; return f.apply(this, a); };
    }

    // Frame GPU time: the page's own TIME_ELAPSED queries, read when it reads
    // them.
    const TIME_ELAPSED = 0x88BF, GPU_DISJOINT = 0x8FBB;
    const timed = new WeakSet();
    let own = false;
    const begin = P.beginQuery;
    P.beginQuery = function (target, q) {
        if (target === TIME_ELAPSED) { timed.add(q); if (!own) S.foreign_queries++; }
        return begin.call(this, target, q);
    };
    const get_parameter = P.getParameter;
    P.getParameter = function (p) {
        const r = get_parameter.call(this, p);
        if (p === GPU_DISJOINT && r) S.disjoint = true;
        return r;
    };
    const query_parameter = P.getQueryParameter;
    P.getQueryParameter = function (q, p) {
        const r = query_parameter.call(this, q, p);
        if (p === this.QUERY_RESULT && timed.has(q)) {
            timed.delete(q);
            if (S.disjoint) S.disjoint = false;
            else {
                const ms = r / 1e6;
                S.gpu_samples++;
                if (ms > S.gpu_ms_max) S.gpu_ms_max = ms;
                if (S.gpu_source === "none") S.gpu_source = own ? "probe" : "page";
            }
        }
        return r;
    };

    // Context loss, on every canvas that makes a WebGL context.
    const get_context = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (kind, ...rest) {
        const ctx = get_context.call(this, kind, ...rest);
        if (ctx && /webgl/.test(kind) && !this.__mp_safety) {
            this.__mp_safety = true;
            this.addEventListener("webglcontextlost", () => S.lost++);
            this.addEventListener("webglcontextrestored", () => S.restored++);
            if (ctx instanceof WebGL2RenderingContext) state(ctx);
        }
        return ctx;
    };

    // Frames: intervals, and the probe's own GPU timing where the page times
    // nothing (after its first 120 frames).
    let last = 0;
    let pending = [];
    let ext;
    const raf = window.requestAnimationFrame.bind(window);
    window.requestAnimationFrame = cb => raf(time => {
        const gl = contexts[0];
        const mine = own && gl && ext && !gl.isContextLost() && pending.length < 4;
        let q = null;
        if (gl && own && ext && !gl.isContextLost()) {
            if (pending.length) gl.getParameter(GPU_DISJOINT);
            while (pending.length && gl.getQueryParameter(pending[0], gl.QUERY_RESULT_AVAILABLE)) {
                const p = pending.shift();
                gl.getQueryParameter(p, gl.QUERY_RESULT);
                gl.deleteQuery(p);
            }
        }
        if (mine) { q = gl.createQuery(); gl.beginQuery(TIME_ELAPSED, q); }
        try { cb(time); } finally {
            if (q) { gl.endQuery(TIME_ELAPSED); pending.push(q); }
        }
    });
    const tick = now => {
        if (last) {
            const dt = now - last;
            if (S.counting && dt > S.interval_max) S.interval_max = dt;
        }
        last = now;
        S.frames++;
        if (S.frames === 120 && S.foreign_queries === 0 && contexts[0]) {
            ext = contexts[0].getExtension("EXT_disjoint_timer_query_webgl2");
            own = !!ext;
            S.own_queries = own;
        }
        raf(tick);
    };
    raf(tick);

    // A coverage build (the optimiser's `__cov` probe): the memory the
    // module marks every function it enters in, read at the end of the play
    // as base64 (`__mp_cov_read()`).
    for (const name of ["instantiate", "instantiateStreaming"]) {
        const orig = WebAssembly[name];
        if (!orig) continue;
        WebAssembly[name] = function (...args) {
            return orig.apply(this, args).then(r => {
                const inst = r && (r.instance || r);
                if (inst && inst.exports && inst.exports.__cov && !window.__mp_cov) window.__mp_cov = inst.exports.__cov;
                return r;
            });
        };
    }
    window.__mp_cov_read = () => {
        if (!window.__mp_cov) return null;
        const b = new Uint8Array(window.__mp_cov.buffer);
        let s = "";
        for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode.apply(null, b.subarray(i, i + 0x8000));
        return btoa(s);
    };

    // The gate's read-out.
    window.__mp_safety_sample = reset => {
        let wasm = 0;
        for (const m of memories) { try { wasm += m.buffer.byteLength; } catch (e) {} }
        S.wasm = wasm;
        if (wasm > S.wasm_peak) S.wasm_peak = wasm;
        let canvas = 0;
        for (const gl of contexts) { try { canvas += gl.drawingBufferWidth * gl.drawingBufferHeight * 4 * 2; } catch (e) {} }
        const gpu = S.gpu + canvas;
        if (gpu > (S.gpu_total_peak || 0)) S.gpu_total_peak = gpu;
        const m = media.find(e => e.src || e.currentSrc);
        const out = {
            wasm: S.wasm, wasm_peak: S.wasm_peak, gpu, gpu_peak: Math.max(S.gpu_total_peak, S.gpu_peak + canvas),
            lost: S.lost, restored: S.restored, frames: S.frames, interval_max: S.interval_max,
            gpu_ms_max: S.gpu_ms_max, gpu_samples: S.gpu_samples, gpu_source: S.gpu_source,
            context_lost_now: contexts.some(gl => gl.isContextLost()),
            loader: document.getElementsByClassName("canvas_loader").length,
            gpu_live: S.gpu + canvas, live: S.live, draws: S.draws, grows: S.grows,
            stats: typeof window.makepad_gpu_stats === "function" ? window.makepad_gpu_stats() : null,
            media: m ? { t: m.currentTime, paused: m.paused, ended: m.ended, duration: m.duration || 0 } : null,
            errors: S.errors,
        };
        if (reset === "counting") S.counting = true;
        return JSON.stringify(out);
    };

    // What holds GPU memory now: the page's own inventory (with owners) when
    // the runtime publishes one, else the probe's, by kind x format x size.
    const format_name = internal => {
        for (const k in WebGL2RenderingContext) {
            if (WebGL2RenderingContext[k] === internal && /^[A-Z0-9_]+$/.test(k) && !/^(UNSIGNED|BYTE|SHORT|INT|FLOAT|HALF)/.test(k)) return k;
        }
        return `0x${internal.toString(16)}`;
    };
    window.__mp_safety_breakdown = async () => {
        if (typeof window.makepad_gpu_inventory === "function") {
            try {
                const inv = await window.makepad_gpu_inventory();
                const groups = new Map();
                for (const t of inv.textures || []) {
                    const key = `${t.dropped ? "dropped, awaiting retirement: " : ""}${t.owner || "?"}: ${t.kind} ${t.format} ${t.w}x${t.h}`;
                    const g = groups.get(key) || { what: key, count: 0, bytes: 0 };
                    g.count++; g.bytes += t.bytes || 0;
                    groups.set(key, g);
                }
                const items = [...groups.values()].sort((a, b) => b.bytes - a.bytes).slice(0, 16);
                for (const [what, bytes] of [["retired textures", inv.retired_texture_bytes], ["retained allocations", inv.retained_allocation_bytes], ["geometry", inv.geometry_bytes]]) {
                    if (bytes) items.push({ what, count: 0, bytes });
                }
                return JSON.stringify({ source: "page", items });
            } catch (e) {}
        }
        const groups = new Map();
        for (const [obj, d] of meta) {
            const m = sizes.get(obj);
            if (!m) continue;
            let bytes = 0;
            for (const b of m.values()) bytes += b;
            const key = `${d.kind} ${format_name(d.internal)} ${d.w}x${d.h}${d.depth > 1 ? "x" + d.depth : ""}`;
            const g = groups.get(key) || { what: key, count: 0, bytes: 0 };
            g.count++; g.bytes += bytes;
            groups.set(key, g);
        }
        const items = [...groups.values()].sort((a, b) => b.bytes - a.bytes).slice(0, 16);
        return JSON.stringify({ source: "probe", items });
    };
})();
