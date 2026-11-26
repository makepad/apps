# `makepad-asset-server` — the standalone Asset Server host

The asset store is inherently **multiplayer**. Stage, Sandbox
and headless workers are all clients of one catalog, and they open and close
independently of each other. So the catalog's lifetime must not be any one
client's lifetime.

Historically the Asset UI *embedded* the server: it took `<root>/server.lock`,
served the catalog, ran the chat broker and the events hub — and every time
that window was closed or rebuilt, every other connected client lost the
store mid-session (`503 state unavailable`). This binary is the fix: the same
service, in its own process, that survives every window.

```
cargo build --release -p makepad-app-asset-server
./target/release/makepad-asset-server --app stage
```

## One asset library per app

Every app has its own store, and apps never share one:

| App | Store root (`paths::app_store_root(app)`) | Library it publishes |
| --- | --- | --- |
| `stage` | `local/asset-library/store` (or `$STAGE_ASSET_ROOT`, `$AI_CONTENT_ASSET_ROOT`) | `local/asset-library` |
| any other, e.g. `sandbox` | `local/asset-library/<app>/store` (or `$<APP>_ASSET_ROOT`) | `local/asset-library/<app>` |

**Apps host their own store in-process** (`embed::resolve`, loopback only,
no beacon). An app attaches instead only when its OWN store is already
served: health at the store's `listen` reports the store's `server-id`, the
store's `server.lock` is held, or a beacon carries that id. So two instances
of one app share a store (the first hosts, the second attaches), and no app
ever joins a server of another app's store.

This binary is a **tool** for running one store out of process. It needs the
store named: `--app <name>` (that app's store and library) or `--root <dir>`
(no library unless `--library` is given). `MAKEPAD_ROOT` relocates the
checkout root; `MAKEPAD_ASSET_LIBRARY` overrides the generated payload
library. App cache directories remain separate.

**Schema upgrades are backed up.** Any host (in-process or this binary)
that opens a store whose catalog is older than its schema first copies the
store directory to `<store>.backup-v<old>-<unix seconds>`, once per old
version, under the store's lock (APFS clones, so it is cheap there). If the
copy fails the store is not opened.

---

## What this process carries

| Part | Where it lives | Notes |
| --- | --- | --- |
| Catalog + CAS, control/data planes | `AssetServer::start` | search, blobs, ranges/ETags, retire |
| Auth, tokens, grants | ” | `--root/admin-token` bootstrapped at start |
| Job queue, worker/lease protocol | ” | plus `GET /v1/job-profiles` |
| Chat broker (+ client-executed tool parking for game sessions) | ” | `--chat-fleet` / LAN fleet discovery |
| Games publish path, operations, import routes | ” | |
| Committed events hub (`/v1/events`) | ” | every client's live view |
| LAN discovery beacon | ” | `--no-beacon` to stay silent |
| Lease janitor + bounded blob GC | ” | runs whether or not anyone polls |
| **ai-content library publisher** | `makepad_asset_importer::watch` | `--library` / `--no-library` |
| **GPU-fleet job coordinator + profile announce** | `makepad_asset_importer::gen_service` | `--no-jobs` / `--no-announce` / `--fleet` |

The last two are the loops the Asset UI used to run *only while it was
hosting*. They are headless — no `Cx`, no window, no GPU surface — so they
belong beside the server, and running them here is what makes a UI-less
deployment a complete fleet citizen. Without the coordinator, jobs any client
enqueues sit at "waiting for agent" forever.

## What deliberately stays client-side

These are **content-deriving** loops that need resources only a UI process
has. They reach the catalog as ordinary clients; moving them here would mean
giving a headless daemon a window.

- **Thumbnail / preview backfill** (the splat + mesh offscreen renders,
  which the retired asset UI ran). Needs a real `Cx`, render passes and
  GPU readback.
- **Classic-game import** (`import_classic.rs`) and **pack import** wizards —
  interactive, and their icon bakes are GPU renders. The one-shot headless
  equivalents already exist as `makepad-asset-importer --import-pack /
  --import-games / --import-music / --import-ai-library` when a batch run is
  wanted.
- **Stems / lyrics analysis bake** (`analysis.rs`) — a large local model on
  the operator's machine, driven from the surface that asks for it.
- **Generation pipelines** driven from the Create surface (`pipeline.rs`) —
  these are *requests*; the queue and the dispatch live server-side.

## Running a store out of process

The default is in-process hosting (above). To run Stage's store as its own
process instead, with every Stage tool attached:

```bash
# 1. the store (leave it running; it survives every UI restart)
./target/release/makepad-asset-server --app stage > /tmp/asset-server.log 2>&1 &

# 2. every client, told never to hold the root itself
export STAGE_ASSET_EMBED=never
./target/release/makepad-stage
```

`STAGE_ASSET_EMBED=never` (aliases: `no`, `off`, `0`, `false`, `attach`,
`client`) makes Stage a pure client: it never takes
`<root>/server.lock`, at startup or during a succession. Without it the app
still attaches when the daemon holds the root — but whichever process starts
*first* wins the lock, so a UI launched before the daemon would put the store
back inside a window.

Stage attaches to it on its own (it serves Stage's store); set
`STAGE_ASSET_EMBED=never` to keep Stage from hosting while it restarts.

### How a client finds the server

Three ways, in order of precedence:

1. `STAGE_ASSET_SERVER=ip:controlport:dataport` (`<APP>_ASSET_SERVER`) — an explicit pin.
2. `<root>/listen`, `<root>/server-id`, `<root>/admin-token` — what the
   daemon writes at startup. This is the attach path, and it is why the
   default bind is `0.0.0.0:0`: the *file* is the address of record.
3. The LAN discovery beacon — how a client on another machine finds it with
   no configuration at all. A client of one app's store pins that store's
   `server-id`; a beacon of any other store is not its server.

## What survives what

- **Closing / rebuilding / crashing any app** — the server keeps serving.
  Every other client stays connected; nothing is lost.
- **Restarting the daemon** — attached clients notice the silence within
  ~3.5 s, say so honestly in the status chip, and rejoin the new process as
  soon as it rewrites `listen`. Cursors and leases are re-established; the
  events journal reports an explicit `gap` rather than a silent hole.
- **A daemon crash with `EmbedPolicy::Auto`** — an attached Stage succeeds
  it (takes the lock, starts the loops, reconnects to itself). This is the
  transition-mode safety net, not the deployment.
- **A daemon crash with `STAGE_ASSET_EMBED=never`** — no app fills the
  vacancy; clients wait and rejoin whoever takes the root next. Restart the
  daemon.
- **Two daemons on one root** — refused immediately and by name
  (`server root: locked by another server process`). One process per root is
  a law: the job routing metadata assumes a single enqueuer, and two writers
  over one WAL catalog would be two sources of recovery truth.

## Bouncing the daemon safely

```bash
# find it (it holds the lock and wrote the listen file)
cat local/asset-library/store/listen        # ip:control:data
pkill -TERM -f 'makepad-asset-server'         # clean shutdown: joins every thread
./target/release/makepad-asset-server --app stage > /tmp/asset-server.log 2>&1 &
```

SIGTERM/SIGINT shut down in order: background loops first (so nothing is
still publishing into a closing catalog), then the planes, then the state
thread. Startup logs what recovery found — `recovered N cas temps / M leases`
— which is the honest report of what the previous life left behind.

## Isolated instances

Never point a scratch instance at the live root, and never let one beacon:

```bash
./target/release/makepad-asset-server \
  --root /tmp/scratch-store --work /tmp/scratch-work \
  --no-beacon --no-jobs --no-library
```

`--no-beacon` keeps peers hunting for the real store from finding the scratch
one; `--no-jobs` keeps it from claiming the real fleet's queued work.

## Tests

`cargo test -p makepad-app-asset-server --release` covers the composition:
the host serves a catalog over real sockets and shuts down cleanly, a second
host on one root is refused by name, a missing library directory never costs
the catalog its server, and the defaults are the deployment defaults;
`embed` covers root choice per app, same-app sharing and cross-app refusal
(real hosts on temp roots), and `backup` the copy before an upgrade. The
parts themselves are covered where they live — `apps/apps/engine/asset/store` (HTTP,
chat, events, jobs, security, operations suites) and `apps/apps/engine/asset/importer`
(watch, coordinator, gen-service). The attach/succession contract is in
`src/embed.rs`.
