# Live model previews

Preview sessions share unfinished geometry without registering a blob, revision
or catalog alias. Open accepts a unique session, a `gen/csg/*` or `gen/drafts/*`
alias and opaque program text charged to the metadata book. It announces the session
immediately, even before a mesh exists. Sessions and parts have no count limit;
part names use lowercase letters, digits and hyphens, up to 32 bytes.

Two books share the store between the owners of live sessions. The draft disk
book charges live part lengths and declared lengths of uploads in flight. Its
default budget is 25% of available volume space plus bytes already in the book,
remeasured at every part admission. The metadata book charges session and part
structures and their strings; its default budget is 1% of physical RAM measured
at startup. `ServerConfig::preview_disk` and `preview_meta` accept
`PreviewBudget::Share(f)` or `Bytes(n)`. If a platform measurement is unavailable,
that book has no budget and the store logs the unavailable measurement once.
Each owner's fair share is the budget divided by the number of live owners,
including the requester; unused shares are lent while the book has room.

Before reading a part body, admission proceeds in this order:

1. A part larger than the whole disk budget is withheld, without eviction.
2. If the part fits, reserve its declared length.
3. Otherwise drop that session's `supersedes` parts, with one removal event.
4. Reclaim parts from other sessions of over-share owners, counting the new
   upload against its author. The uploader's own sessions come first, then
   other owners, oldest activity first and largest parts first. A session
   stays open with its program and lease, so receivers can show its cage.
5. If space is still occupied by other owners' fair shares, wait without a
   reservation. Failed and abandoned uploads release their reservation.

A wait returns **503** with `error: "model preview draft space full"`, `len`,
`used`, `budget` and `share`. A withheld part returns **507** with
`error: "model preview part larger than this store's draft space"`, `len` and
`budget`. Both close the connection before reading a body byte. Disk-full
spooling also returns 503. The client sends `Expect: 100-continue`: a final
refusal returns without reading its body source; 100 permits the upload. If no
complete response head arrives within one second, it sends the body anyway and
accepts a late 100 before the final response. Early refusal sockets are never
pooled. API errors retain 503/507 and the server's sanitized explanation.

The PUT's optional `supersedes` query is a comma-separated list of validated
part names in the same session. Without disk pressure these stay live during the
upload. Success emits ONE event carrying the new part and removal of superseded
parts that still existed. Early pressure removals emit their own event before
the upload. Sandbox sends its older candidates here, keeps at most two candidate
parts before an upload, and re-uploads the same mesh after its current room
part is evicted. A swap that carries a new part is not an eviction.

Metadata admission on open, program/name edits and part changes lapses other
sessions of owners over their metadata share, the requester's own sessions
first, oldest activity first. Lapses emit ordinary clear events. If the change
still cannot fit, it returns **503**, `"model preview metadata space full"`.
Pressure steps are logged once per session and step kind.

Part PUT requires Content-Length and streams into an anonymous file under the
store's `previews/` directory while computing its `pmesh_<sha256>` token. The
spool is accounted by the disk book and never enters CAS or fsyncs.
Replacing, removing, clearing or expiring a part withdraws its token immediately;
readers already holding its file can finish. Journal events and snapshots keep
only metadata, including each part's required `bytes` length. Files are unlinked
on Unix or deleted on close on Windows, and startup removes leftover names.
Nothing survives a server restart.

Mesh GET streams `model/gltf-binary`, supports HEAD and single byte ranges, and
returns 416 for an unsatisfiable range. Upload reads and client fetches use stall
timeouts, with no total body deadline. The client allocates the declared response
length once and hashes while reading, refusing a digest that differs from the
token. The client streams an upload from its source in 1 MiB writes, each with
its own write timeout, and stops between writes when the source is abandoned. No
fixed part-size constant applies on either side; the store admits against its
current disk budget, and a declared length the system cannot allocate fails that fetch.
A receiver can admit a part from its `bytes` before fetching it: Sandbox charges
that length to its device memory book and, when it does not fit, fetches nothing
and shows the draft's cage (or keeps the saved model) until it does.

Sessions record their opening principal. Upload progress refreshes every live
session belonging to that owner, at most once per second. The lease is 120 seconds
on the server's monotonic clock. Send an empty metadata delta every 30 seconds
while editing; it renews the session without an event. Reopen a missing session.

Event vocabulary 4 or newer provides an atomic cursorless bootstrap of all live
sessions and their tail cursor, independent of delta page size. Resume from that
cursor; after a gap, discard old preview state and bootstrap again.
`CatalogSubscriber` does this automatically. Sandbox filters program scope before
fetching, uses the greatest `draft-<u64 generation>` part, and clears previews on
close, cancellation, scope departure or successful publication. A visible preview
never implies durable publication or placement.

## Access

Open, delta, part PUT and clear require `preview_publish` on the alias namespace;
`asset_publish` does not grant preview rights, and preview rights do not grant
durable publication. Delta and PUT require the opening principal. Clear also
requires that owner, unless the caller has `asset_quarantine` on the namespace
for moderation. The bootstrap admin retains its capability bypass.

Open may set an opaque `audience`: 1–128 ASCII bytes from `[A-Za-z0-9_.:-]`.
Subscribers name their audiences in one `preview_audience` query parameter,
comma-separated (an audience contains no comma). Only matching preview events and bootstrap entries are selected;
audience-less sessions remain visible to all authenticated subscribers, and
other catalog events are unchanged. Every preview event carries its session's
`preview_audience` when set, including clear and expiry events.

Mesh GET/HEAD must name the session's audience with `audience`; a missing or
wrong audience returns 404, just like an unknown token. All reads still require
authentication. Audience keys are bearer knowledge, not principal grants.
Sandbox uses `pa1:` plus hex SHA-256 of `game\0sub\0room`, subscribes for every
pane's scope, and retains its program-scope check before fetching. The room
component is the invite-key digest for a network room; the invite stays local.
