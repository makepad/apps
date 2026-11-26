// Loads a film bundle (makepad-story-player-bundle) into Cache Storage, fonts
// first, with a progress feed the page and the player read.
//
//   import { register_bundle_cache, load_bundle } from './bundle_loader.js';
//   register_bundle_cache('./bundle_sw.js');          // once, early
//   const { manifest, url } = await load_bundle('./film/');
//   url('ast_x:arev_y')  // the hashed URL of a logical name
//
// manifest.json is fetched fresh (`no-cache`: revalidated every visit);
// every other file is immutable and named by its content, read from Cache
// Storage when there and fetched (then kept) when not. Entries of earlier
// bundles at the same base are deleted once the new bundle is complete.
//
// Progress: `window.makepad_bundle_progress` = {state: 'manifest'|'loading'|
// 'done'|'error', total, loaded, cached, files, files_done, current:
// [names], ms}, also sent as a `makepad-bundle-progress` event on window.
// Bytes are the files' sizes (decoded), so a compressed transfer counts the
// same. A player that fetches the files itself instead gets the same totals
// from manifest.files[..].bytes and its own fetch progress.
export const CACHE = 'makepad-immutable-v1';
const ALIASES = 'makepad-aliases.json';
const ORDER = { fonts: 0, data: 1, docs: 2, textures: 3, audio: 4 };

export async function register_bundle_cache(sw_url) {
    if (!('serviceWorker' in navigator)) return null;
    try {
        return await navigator.serviceWorker.register(sw_url);
    } catch (e) {
        console.warn('bundle cache: no service worker', e);
        return null;
    }
}

function progress_feed() {
    const p = { state: 'manifest', total: 0, loaded: 0, cached: 0, files: 0, files_done: 0, current: [], ms: 0 };
    const t0 = performance.now();
    let queued = false;
    p.emit = (now) => {
        p.ms = performance.now() - t0;
        if (now) {
            window.dispatchEvent(new CustomEvent('makepad-bundle-progress', { detail: p }));
        } else if (!queued) {
            queued = true;
            requestAnimationFrame(() => { queued = false; window.dispatchEvent(new CustomEvent('makepad-bundle-progress', { detail: p })); });
        }
    };
    window.makepad_bundle_progress = p;
    return p;
}

async function load_file(cache, url, name, bytes, p) {
    const hit = await cache.match(url);
    if (hit) {
        p.loaded += bytes;
        p.cached += bytes;
        p.files_done += 1;
        p.emit();
        return;
    }
    p.current.push(name);
    const response = await fetch(url);
    if (!response.ok) throw new Error(`${name}: HTTP ${response.status}`);
    const reader = response.body.getReader();
    const chunks = [];
    for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        chunks.push(value);
        p.loaded += value.byteLength;
        p.emit();
    }
    const type = response.headers.get('content-type') || 'application/octet-stream';
    await cache.put(url, new Response(new Blob(chunks, { type }), { headers: { 'Content-Type': type } }));
    p.current.splice(p.current.indexOf(name), 1);
    p.files_done += 1;
    p.emit();
}

export async function load_bundle(base, { parallel = 6 } = {}) {
    const p = progress_feed();
    p.emit(true);
    const root = new URL(base, location.href);
    try {
        const manifest = await (await fetch(new URL('manifest.json', root), { cache: 'no-cache' })).json();
        // Each hashed file once (logical names may share one), fonts first, then the small before the big.
        const files = new Map();
        for (const entry of Object.values(manifest.files)) files.set(entry.file, entry);
        const queue = [...files.values()].sort((a, b) => (ORDER[a.kind] ?? 9) - (ORDER[b.kind] ?? 9) || a.bytes - b.bytes || (a.file < b.file ? -1 : 1));
        p.state = 'loading';
        p.total = queue.reduce((n, f) => n + f.bytes, 0);
        p.files = queue.length;
        p.emit(true);
        const cache = await caches.open(CACHE);
        let next = 0;
        const worker = async () => {
            while (next < queue.length) {
                const f = queue[next++];
                await load_file(cache, new URL(f.file, root).href, f.file, f.bytes, p);
            }
        };
        await Promise.all(Array.from({ length: Math.min(parallel, queue.length) }, worker));
        const url = (logical) => manifest.files[logical] && new URL(manifest.files[logical].file, root).href;
        // Fonts by the web path the runtime fetches them at, for the service worker.
        const aliases = {};
        for (const [logical, entry] of Object.entries(manifest.files)) if (entry.kind === 'fonts') aliases[logical] = url(logical);
        const scope = navigator.serviceWorker && (await navigator.serviceWorker.getRegistration())?.scope;
        await cache.put(new URL(ALIASES, scope || location.href).href, new Response(JSON.stringify(aliases), { headers: { 'Content-Type': 'application/json' } }));
        navigator.serviceWorker?.controller?.postMessage({ type: 'makepad-bundle-aliases', aliases });
        // Earlier bundles at this base: gone once this one is whole.
        const keep = new Set([...files.keys()].map((f) => new URL(f, root).href));
        for (const request of await cache.keys()) {
            if (request.url.startsWith(root.href) && !keep.has(request.url)) await cache.delete(request);
        }
        p.state = 'done';
        p.emit(true);
        return { manifest, url, base: root.href };
    } catch (e) {
        p.state = 'error';
        p.error = String(e);
        p.emit(true);
        throw e;
    }
}
