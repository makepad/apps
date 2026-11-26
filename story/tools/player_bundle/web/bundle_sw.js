// The player's content-addressed cache (a service worker served beside the
// player's index.html, so its scope covers the app and the film bundle).
//
// - A file named by its content (`name.<16 hex>.ext`: the bundle's files,
//   the hashed wasm and data of a `cargo makepad` build) never changes, so
//   it is served from Cache Storage when there, else fetched once and kept.
// - A crate resource the wasm runtime fetches by its unhashed web path
//   (`<app>/makepad_widgets/resources/Inter.ttf`) is served as the bundle's
//   hashed copy of that file when the bundle names it (the alias table the
//   page's loader stores, see bundle_loader.js), so fonts come from Cache
//   Storage on every visit after the first.
// - Everything else (index.html, manifest.json, the runtime's js) goes to
//   the network untouched: those are what decide which hashed files to use.
const CACHE = 'makepad-immutable-v1';
const ALIASES = 'makepad-aliases.json';
const HASHED = /\.[0-9a-f]{16}\.[A-Za-z0-9]+$/;

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));

let aliases = null;
async function alias_table() {
    if (aliases) return aliases;
    try {
        const cache = await caches.open(CACHE);
        const hit = await cache.match(new URL(ALIASES, self.registration.scope).href);
        aliases = hit ? await hit.json() : {};
    } catch (_e) {
        aliases = {};
    }
    return aliases;
}
self.addEventListener('message', (event) => {
    if (event.data && event.data.type === 'makepad-bundle-aliases') aliases = event.data.aliases || {};
});

// The hashed URL a resource path is an alias of: the longest path suffix the table names.
function lookup(table, pathname) {
    const parts = pathname.split('/');
    for (let i = 1; i < parts.length; i++) {
        const hit = table[parts.slice(i).join('/')];
        if (hit) return hit;
    }
    return null;
}

const inflight = new Map();
async function cache_first(url) {
    const cache = await caches.open(CACHE);
    const hit = await cache.match(url);
    if (hit) return hit;
    if (inflight.has(url)) return (await inflight.get(url)).clone();
    const job = fetch(url).then(async (response) => {
        if (response.ok && response.status === 200) await cache.put(url, response.clone());
        return response;
    });
    inflight.set(url, job);
    try {
        return (await job).clone();
    } finally {
        inflight.delete(url);
    }
}

self.addEventListener('fetch', (event) => {
    const request = event.request;
    if (request.method !== 'GET') return;
    const url = new URL(request.url);
    if (url.origin !== self.location.origin) return;
    if (HASHED.test(url.pathname)) {
        event.respondWith(cache_first(url.href));
        return;
    }
    if (url.pathname.includes('/resources/')) {
        event.respondWith((async () => {
            const target = lookup(await alias_table(), url.pathname);
            return target ? cache_first(target) : fetch(request);
        })());
    }
});
