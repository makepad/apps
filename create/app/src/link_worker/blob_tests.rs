//! Two homes, real authenticated streams, and a simulated clock. Each store turn runs the
//! worker's normal port against a real town directory, so holding that turn holds write credit.
use super::*;
use crate::model::{tests::{file_reference, shared_town, ShareHomes}, Action, Model, Origin, ShareState};
use crate::store_worker::{SimStore, StoredFile};
use makepad_network::p2p::natsim::{LinkConfig, Net};
use makepad_town_core::blob::{self, Body};
use makepad_town_core::event::{Id, TownId};
use std::collections::BTreeSet;
use std::ops::Range;

fn attempt(peer: PublicKey) -> Attempt { Attempt { carrier: Carrier::Peer(peer), generation: 1 } }

const CHUNK: usize = 3;
const AUTO: u64 = 9;

#[derive(Default)]
struct ObservedPump {
    attachment: Pump,
    received_blobs: Vec<Vec<u8>>,
    hold_blob_output: bool,
}
impl std::ops::Deref for ObservedPump {
    type Target = Pump;
    fn deref(&self) -> &Pump { &self.attachment }
}
impl std::ops::DerefMut for ObservedPump {
    fn deref_mut(&mut self) -> &mut Pump { &mut self.attachment }
}

struct Side {
    model: Model,
    store: SimStore,
    conn: Connection,
    slot: hub::Slot,
    pump: ObservedPump,
    addr: SocketAddr,
    sync: VecDeque<Vec<u8>>,
    held: bool,
    blobs: blobs::BlobPump,
    linked: Option<Attempt>,
}

impl Side {
    fn new(mut model: Model, mut store: SimStore, peer: PublicKey, addr: SocketAddr, to: SocketAddr, nonce: u8) -> Self {
        let mode = Mode::Reconnect(Reconnect { device: model.device_key(), peers: vec![Pin {
            device: peer, min_version: makepad_town_core::link::VERSION,
        }]});
        let conn = Connection::new(mode, to, [7; 8], [nonce; 72], 0).unwrap();
        let slot = hub::Slot::new(model.device_key(), Start::Known {pin:Pin {device:peer, min_version:makepad_town_core::link::VERSION}, path:Remembered {public:Some(to), lan:None}});
        let (blobs, endpoint) = blobs::BlobPump::new(model.me(), CHUNK, || {});
        model.bind_blobs(endpoint, store.attach());
        Self { model, store, blobs, linked: None, conn, slot, pump: ObservedPump::default(),
            addr, sync: VecDeque::new(), held: false }
    }

    fn turn(&mut self, net: &mut Net, hold_fold: bool) {
        let now = net.now();
        if self.conn.phase() == &Phase::Linked && self.linked.is_none() {
            let peer = self.conn.peer().unwrap().device;
            let at = attempt(peer);
            self.blobs.attach(at);
            self.model.attach_blob_view(at);
            self.linked = Some(at);
        }
        if !hold_fold {
            while let Some(msg) = self.sync.pop_front() { self.model.sync_receive(&msg).unwrap(); }
        }
        if self.linked.is_some() {
            let sync = self.model.sync_outgoing();
            assert!(self.model.publish_blob_policy(AUTO));
            for msg in sync { self.conn.send(Kind::Sync, &msg); }
        }
        if !self.held { self.store.turn(); }
        let writable = self.conn.writable(Kind::Blob) && !self.pump.hold_blob_output;
        self.blobs.pump(now);
        if writable {
            if let Some(bytes) = self.linked.and_then(|at| self.blobs.poll_send(at)) { self.conn.send(Kind::Blob, &bytes); }
        }
        let (active, events) = self.slot.poll_connection(&mut self.conn, Now {ms:now, unix_secs:0});
        assert!(active, "the store simulation kept its authenticated connection");
        for event in events {
            match event {
                Event::Send(to, bytes) => net.send(self.addr, to, bytes),
                Event::Sync(msg) => self.sync.push_back(msg),
                Event::Blob(bytes) => { self.blobs.receive(self.linked.unwrap(), &bytes, now); self.pump.received_blobs.push(bytes); }
                Event::CallReady => self.pump.call.pending.extend(self.slot.take_call_frames().map(|(_, body)| body)),
                _ => {}
            }
        }
    }

    fn trace<'a>(&'a self) -> impl Iterator<Item = blob::Message<'a>> + 'a {
        self.pump.received_blobs.iter().map(|bytes| blob::Message::decode(bytes).unwrap())
    }

    fn wants(&self, id: Id) -> Vec<Vec<Range<u64>>> {
        self.trace().filter_map(|m| match m.body {
            Body::Want(ranges) if m.id == id => Some(ranges.iter().collect()), _ => None,
        }).collect()
    }

    fn stored(&self, town: TownId, id: Id) -> Option<Vec<u8>> {
        let dir = self.model.home.blobs_dir().join(makepad_town_core::hex(&town));
        let name = makepad_town_core::hex(&id);
        if !dir.join(format!("{name}.tree")).is_file() { return None; }
        std::fs::read(dir.join(name)).ok()
    }
}

struct Pair {
    net: Net,
    a: Side,
    b: Side,
    delay_founder_fold: bool,
}

impl Pair {
    fn new(a: Model, b: Model, sa: SimStore, sb: SimStore) -> Self {
        let mut net = Net::new(42, LinkConfig { latency_ms: 1, jitter_ms: 0, loss_per_mille: 0 });
        let ips = [Ipv4Addr::new(192, 0, 2, 1), Ipv4Addr::new(192, 0, 2, 2)];
        for ip in ips { net.add_host(ip, None); }
        let [aa, ba] = ips.map(|ip| SocketAddr::from((ip, 4040)));
        let (ak, bk) = (a.me(), b.me());
        Self { a: Side::new(a, sa, bk, aa, ba, 1), b: Side::new(b, sb, ak, ba, aa, 2), net, delay_founder_fold: false }
    }

    fn turn(&mut self) {
        let now = self.net.now() + 5;
        for d in self.net.advance_to(now) {
            let conn = if d.to == self.a.addr { &mut self.a.conn } else { &mut self.b.conn };
            conn.handle(d.from, &d.bytes, now);
        }
        let hold = self.delay_founder_fold && self.b.model.share_state() == ShareState::Shared(self.a.model.open_id());
        self.a.turn(&mut self.net, hold);
        self.b.turn(&mut self.net, false);
    }

    fn until(&mut self, reason: &str, mut reached: impl FnMut(&mut Pair) -> bool) {
        for _ in 0..2000 {
            if reached(self) { return; }
            self.turn();
        }
        panic!("{reason}: phases {:?}/{:?}, towns {:?}/{:?}, blob messages {}/{}",
            self.a.conn.phase(), self.b.conn.phase(), self.a.model.share_state(), self.b.model.share_state(),
            self.a.trace().count(), self.b.trace().count());
    }
}

fn add(model: &mut Model, store: &mut SimStore, bytes: &[u8]) -> StoredFile {
    let file = store.add(model.open_id(), bytes);
    file_reference(model, file);
    file
}

#[test]
fn delayed_founder_fold_reissues_the_lost_want_and_large_files_wait_for_a_view() {
    let homes = ShareHomes::new("blob-join");
    let (mut a, b) = homes.pair();
    let town = a.open_id();
    let other = b.open_id();
    let mut sa = SimStore::open(a.home.clone(), town, CHUNK);
    let sb = SimStore::open(b.home.clone(), b.open_id(), CHUNK);
    let small = add(&mut a, &mut sa, b"small");
    let large = add(&mut a, &mut sa, b"a longer file with several chunks");
    a.offer_town(false).unwrap();
    let mut pair = Pair::new(a, b, sa, sb);
    pair.until("offer delivered", |p| matches!(p.b.model.share_state(), ShareState::Incoming(_)));
    let shown = pair.b.model.offer_prompt().expect("the offer is shown");
    assert!(pair.b.model.answer_prompt(shown, true));
    pair.delay_founder_fold = true;
    pair.until("founder received the first WANT", |p| !p.a.wants(small.id).is_empty());
    assert!(!pair.a.model.state.active_member(&pair.b.model.me()));
    assert!(!pair.b.trace().any(|m| matches!(m.body, Body::Ready | Body::Missing | Body::Chunk { .. })));
    pair.delay_founder_fold = false;
    pair.until("automatic small file verified", |p| p.b.stored(town, small.id).is_some());
    assert_eq!(pair.b.stored(town, small.id).unwrap(), b"small");
    assert!(pair.a.wants(small.id).len() >= 2);
    assert_eq!(pair.b.trace().filter(|m| matches!(m.body, Body::Ready)).count(), 1);
    assert!(pair.a.wants(large.id).is_empty());
    pair.b.model.open_town(other).unwrap();
    assert_eq!(pair.b.model.sync_town(), Some(town), "the transfer continues in the background town");
    pair.b.model.want_file(town, large.id).unwrap();
    pair.until("first requested window reached the founder", |p| !p.a.wants(large.id).is_empty());
    let n = large.size.div_ceil(CHUNK as u64);
    pair.until("verified bytes before the page closes", |p| p.b.store.absent(town, large.id) == [1..n]);
    pair.b.model.release_file(town, large.id);
    pair.until("withdrawal reaches the server in order", |p| p.a.wants(large.id).last().is_some_and(Vec::is_empty));
    let remaining = pair.b.store.absent(town, large.id);
    assert!(remaining.first().unwrap().start >= 1 && !remaining.is_empty());
    pair.b.model.want_file(town, large.id).unwrap();
    pair.until("viewed file verified", |p| p.b.stored(town, large.id).is_some());
    assert_eq!(pair.b.stored(town, large.id).unwrap(), b"a longer file with several chunks");
    pair.until("page sees Done", |p| matches!(p.b.model.file_transfer(town, large.id), Some(blob::lane::Status::Done {..})));

    // A verified read discovers disk damage. The other home holds a good copy, so the
    // server can recover the single lost chunk even though this file exceeds the auto policy.
    let path = pair.a.model.home.blobs_dir().join(makepad_town_core::hex(&town)).join(makepad_town_core::hex(&large.id));
    let original = pair.a.stored(town, large.id).unwrap();
    let mut damaged = original.clone();
    damaged[CHUNK] ^= 1;
    std::fs::write(path, damaged).unwrap();
    let want = blob::Message { town, id: large.id, body: Body::Want(blob::Ranges::Local(&[1..2])) };
    pair.b.conn.send(Kind::Blob, &want.encode().unwrap());
    pair.until("damaged chunk reported missing", |p| p.b.trace().any(|m| m.id == large.id && matches!(m.body, Body::Missing)));
    pair.until("damaged chunk repaired from the other home", |p| p.a.stored(town, large.id) == Some(original.clone()));
    assert_eq!(pair.b.wants(large.id).first().unwrap(), &[1..2]);
}

#[test]
fn a_reference_folded_after_want_wakes_the_fetch_even_with_missing_still_queued() {
    let (_homes, a, b) = shared_town("blob-late-reference");
    let town = a.open_id();
    let mut sa = SimStore::open(a.home.clone(), town, CHUNK);
    let sb = SimStore::open(b.home.clone(), town, CHUNK);
    let sent_first = sa.add(town, b"one");
    let still_queued = sa.add(town, b"two");
    let mut pair = Pair::new(a, b, sa, sb);
    pair.until("both town views opened", |p| [&p.a, &p.b].iter().all(|side|
        side.trace().any(|m| m.town == town && matches!(m.body, Body::Ready))));

    for (file, bytes, hold_output) in [(sent_first, b"one", false), (still_queued, b"two", true)] {
        let start = pair.b.trace().count();
        pair.delay_founder_fold = true;
        pair.a.pump.hold_blob_output = hold_output;
        // B has the signed reference and lacks the bytes. Delay only A's fold of B's Sync
        // message. Its already-open Blob lane must answer using the view it currently has.
        file_reference(&mut pair.b.model, file);
        pair.until("A admitted the automatic WANT", |p| !p.a.wants(file.id).is_empty());
        assert!(!pair.a.model.references.reachable(&file.id));
        let missing = |p: &Pair| p.b.trace().skip(start).any(|m| m.id == file.id && matches!(m.body, Body::Missing));
        if !hold_output {
            pair.until("B received MISSING before the fold", |p| missing(p));
        }
        pair.delay_founder_fold = false;
        pair.until("A folded B's first reference", |p| p.a.model.references.visible(&file.id, &p.a.model.state, None));
        if hold_output {
            assert!(!missing(&pair), "the earlier denial and the fold's READY are both awaiting handoff");
            pair.a.pump.hold_blob_output = false;
        }

        pair.until("B completed the file without a view request", |p| p.b.stored(town, file.id).is_some());
        assert_eq!(pair.b.stored(town, file.id).unwrap(), bytes);
        let replies: Vec<_> = pair.b.trace().skip(start).filter_map(|m| match m.body {
            Body::Missing if m.id == file.id => Some("MISSING"),
            Body::Ready if m.town == town => Some("READY"),
            Body::Chunk { .. } if m.id == file.id => Some("CHUNK"),
            _ => None,
        }).collect();
        assert_eq!(replies, ["MISSING", "READY", "CHUNK"], "a stale denial cannot put the refreshed fetch to sleep");
        assert_eq!(pair.a.wants(file.id).iter().filter(|ranges| !ranges.is_empty()).count(), 2);
    }
}

#[test]
fn held_store_leaves_call_and_sync_live_then_reconnect_and_restart_use_only_disk_gaps() {
    let (homes, mut a, b) = shared_town("blob-resume");
    let town = a.open_id();
    let mut sa = SimStore::open(a.home.clone(), town, CHUNK);
    let sb = SimStore::open(b.home.clone(), town, CHUNK);
    let bytes = b"enough chunks for three short transfers";
    let file = add(&mut a, &mut sa, bytes);
    let mut pair = Pair::new(a, b, sa, sb);
    pair.until("file reference arrived", |p| p.b.model.references.reachable(&file.id));
    pair.b.model.want_file(town, file.id).unwrap();
    pair.until("first window", |p| !p.a.wants(file.id).is_empty());
    pair.b.held = true;
    let chat = pair.a.model.act(Action::PostChat { text: "while the disk waits".into(), to_ai: false, reply_to: None }, Origin::Ui).unwrap();
    pair.a.conn.send(Kind::Call, b"call control");
    pair.until("both higher lanes passed the stalled blob", |p| {
        p.b.model.state.message(&chat).is_some() && p.b.pump.call.pending.front().is_some_and(|m| m == b"call control")
            && p.b.trace().filter(|m| m.id == file.id && matches!(m.body, Body::Chunk { .. })).count() == 4
    });
    for _ in 0..60 { pair.turn(); }
    assert!(pair.a.wants(file.id).iter().all(|ranges| ranges == &[0..4]), "held writes create no new credit");
    let n = file.size.div_ceil(CHUNK as u64);
    assert_eq!(pair.b.store.absent(town, file.id), [0..n]);
    pair.b.held = false;
    for (cut, restart) in [(2, false), (5, true)] {
        pair.until("verified prefix before cut", |p| p.b.store.absent(town, file.id) == [cut..n]);
        let Pair { a, b, .. } = pair;
        let Side { model: mut a, store: sa, .. } = a;
        let Side { model: mut b, store: mut sb, .. } = b;
        a.sync_stop();
        b.sync_stop();
        if restart {
            sb.flush();
            drop((b, sb));
            b = Model::open(&homes.0.join("b")).unwrap().0;
            sb = SimStore::open(b.home.clone(), town, CHUNK);
        }
        assert_eq!(sb.checked(), if restart { cut } else { 0 }, "only reopening rehashes the prefix");
        a.sync_start(&b.me());
        b.sync_start(&a.me());
        b.want_file(town, file.id).unwrap();
        pair = Pair::new(a, b, sa, sb);
        pair.until("resumed WANT", |p| !p.a.wants(file.id).is_empty());
        assert_eq!(pair.a.wants(file.id)[0], [cut..(cut + 4).min(n)]);
    }
    pair.until("every remaining chunk verified", |p| p.b.stored(town, file.id).is_some());
    assert!(pair.a.wants(file.id).iter().flatten().all(|r| r.start >= 5));
    assert_eq!(pair.b.stored(town, file.id).unwrap(), bytes);
}

#[test]
fn clip_poster_arrives_automatically_and_get_transfers_and_releases_the_recording() {
    use crate::model::tests::{exchange, join};
    use crate::view::{ChatRow, LocalClock, Workspace};
    use makepad_town_core::event::{Change, Value};
    use std::sync::mpsc;

    let homes = ShareHomes::new("clip-get");
    let mut ana = Model::open(&homes.0.join("ana")).unwrap().0;
    let mut ben = Model::open(&homes.0.join("ben")).unwrap().0;
    ana.pin_linked(&ben.me(), 3, "Ben").unwrap();
    ben.pin_linked(&ana.me(), 3, "Ana").unwrap();
    ana.sync_start(&ben.me());
    ben.sync_start(&ana.me());
    let town = join(&mut ana, &mut ben, false);
    let bytes = vec![42; 2 * 1024 * 1024];
    let (media, poster) = {
        let mut store = SimStore::open(ana.home.clone(), town, blob::CHUNK);
        (store.add(town, &bytes), store.add(town, b"poster"))
    };
    let (signal, events) = mpsc::sync_channel(1);
    let wake = || {
        let signal = signal.clone();
        move || { let _ = signal.try_send(()); }
    };
    ana.start_store(crate::store_worker::test_pool(), wake()).unwrap();
    ben.start_store(crate::store_worker::test_pool(), wake()).unwrap();
    while !ana.complete_blobs(&town).is_some_and(|set| set.contains(&media.id) && set.contains(&poster.id)) {
        events.recv().unwrap();
        ana.drain_store();
    }
    let id = ana.act(Action::PostClip { project: Some(ana.project().id), title: "Walkthrough".into(),
        media: (media.size, media.root), poster: Some((poster.size, poster.root)), duration_ms: 84_000,
        width: 640, height: 480, media_kind: "video/mp4".into(), session: None }, Origin::Ui).unwrap();
    exchange(&mut ana, &mut ben);
    let row = |view: &Workspace| view.rows.iter()
        .find_map(|r| match r { ChatRow::Clip(c) if c.id.1 == id => Some(c.clone()), _ => None }).expect("the timeline contains the clip");
    let mut view = Workspace::read(&ben, false, &LocalClock::default());
    let before = row(&view);
    assert!(!view.refresh_clips(&ben, [(town, [9; 32])], true) && Arc::ptr_eq(&before, &row(&view)),
        "an unrelated transfer and unchanged holdings leave the row alone");

    // The normal link pumps exchange encoded blob messages; the real store workers wake
    // the test on disk I/O. Only the link status clock advances in simulated time.
    let (ab, ba) = (attempt(ben.me()), attempt(ana.me()));
    let (mut a, endpoint) = blobs::BlobPump::new(ana.me(), blob::CHUNK, wake());
    ana.attach_blobs(endpoint);
    a.attach(ab);
    ana.attach_blob_view(ab);
    let (mut b, endpoint) = blobs::BlobPump::new(ben.me(), blob::CHUNK, wake());
    ben.attach_blobs(endpoint);
    b.attach(ba);
    ben.attach_blob_view(ba);
    let mut now = 0;
    let mut requested = false;
    let mut progress = false;
    loop {
        ana.drain_store();
        let snapshot = ben.drain_store().changed;
        if snapshot { ben.release_complete_clips(); }
        ana.publish_blobs();
        ben.publish_blobs();
        let changes = ben.take_transfer_changes();
        let media_changed = changes.contains(&(town, media.id));
        view.refresh_clips(&ben, changes, snapshot);
        if media_changed && matches!(ben.file_transfer(town, media.id), Some(blob::lane::Status::Progress { have, .. }) if *have > 0) {
            let clip = row(&view);
            let words = &clip.media.as_ref().unwrap().state;
            assert!(words.contains(" of ") && words.ends_with(" from Ana"), "verified progress names Ana as its carrier: {words}");
            progress = true;
        }
        if !requested && ben.complete_blobs(&town).is_some_and(|set| set.contains(&poster.id)) {
            let clip = row(&view);
            let details = clip.media.as_ref().unwrap();
            assert_eq!(std::fs::read(details.poster.as_ref().expect("poster has a local path")).unwrap(), b"poster",
                "the small poster arrives without pressing Get");
            assert!(details.playable.is_none() && details.get && details.state == "On Ana's computer",
                "a large recording waits on the author's computer until Get");
            assert_eq!((details.height, details.length.as_str()), (180., "1:24"), "the box reserves the recorded aspect");
            ben.get_clip(town, details.blob).expect("the Get button requests the recording");
            view.refresh_clips(&ben, [(town, details.blob)], false);
            assert!(row(&view).media.as_ref().unwrap().state.starts_with("Waiting for Ana"), "Get immediately shows a pending request");
            requested = true;
            continue
        }
        if requested && ben.complete_blobs(&town).is_some_and(|set| set.contains(&media.id)) { break }
        now += 100;
        let mut changed = a.pump(now) | b.pump(now);
        while let Some(msg) = a.poll_send(ab) { b.receive(ba, &msg, now); changed = true; }
        while let Some(msg) = b.poll_send(ba) { a.receive(ab, &msg, now); changed = true; }
        if !changed { events.recv().unwrap(); }
    }
    let clip = row(&view);
    let details = clip.media.as_ref().unwrap();
    assert!(progress, "a verified partial recording produces progress before completion");
    assert!(!details.get && details.state == "On this computer", "a completed recording no longer offers Get");
    assert_eq!(std::fs::read(details.playable.as_ref().expect("Play has a local source")).unwrap(), bytes, "Play opens the verified recording");
    assert!(!ben.file_wanted(town, media.id), "completion releases the window's want");

    ben.act(Action::SetFields { changes: vec![Change { entity: id, field: makepad_town_core::entity::field::DELETED,
        value: Value::Bool(true) }], label: None }, Origin::Ui).unwrap();
    exchange(&mut ben, &mut ana);
    let removed = row(&Workspace::read(&ana, false, &LocalClock::default()));
    assert!(removed.media.is_none() && removed.title == "Ben removed a clip", "deletion shows only the name of the delete's signer");
    ana.stop_store();
    ben.stop_store();
    while !ana.store_stopped() || !ben.store_stopped() {
        ana.drain_store();
        ben.drain_store();
        if !ana.store_stopped() || !ben.store_stopped() { events.recv().unwrap(); }
    }
}

const PNG_1X1: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01,
    0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41,
    0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f, 0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

#[test]
fn dropped_files_are_told_apart_by_their_bytes_and_a_clip_may_have_no_project() {
    use crate::store_worker::CaptureFormat as Kind;
    let mp4 = [b"\0\0\0\x18ftypisom\0\0\x02\0isomiso2".as_slice(), &[0; 8]].concat();
    let mov = [b"\0\0\0\x14ftypqt  \0\0\0\0qt  ".as_slice(), &[0; 8]].concat();
    for (what, bytes, want) in [
        ("a PNG", PNG_1X1.to_vec(), Some(Kind::Png)), ("a JPEG", vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16], Some(Kind::Jpeg)),
        ("a GIF87a", b"GIF87a\x01\0\x01\0".to_vec(), Some(Kind::Gif)), ("a GIF89a", b"GIF89a\x01\0\x01\0".to_vec(), Some(Kind::Gif)),
        ("an MP4", mp4.clone(), Some(Kind::Mp4)), ("a MOV", mov.clone(), Some(Kind::Mov)),
        ("a HEIC photo", b"\0\0\0\x18ftypheic\0\0\0\0mif1".to_vec(), None), ("an M4A song", b"\0\0\0\x18ftypM4A \0\0\0\0M4A ".to_vec(), None),
        ("an AVIF photo", b"\0\0\0\x18ftypavif\0\0\0\0mif1".to_vec(), None),
        ("text", b"hello there, not media".to_vec(), None), ("a cut PNG", PNG_1X1[..5].to_vec(), None),
        ("an empty file", Vec::new(), None), ("a WebP", b"RIFF\0\0\0\0WEBPVP8 ".to_vec(), None),
    ] {
        assert_eq!(Kind::sniff(&bytes), want, "{what} is told by its bytes");
    }

    // Two computers: a picture and two videos dropped on Ana's chat post to the town with no
    // project, the files stay where they were, a lying name does not decide the kind, and Ben's
    // chat shows each row.
    use crate::model::tests::{exchange, join};
    use crate::view::{ChatRow, LocalClock, Workspace};
    use makepad_town_core::entity::{field, kind};
    use makepad_town_core::event::Value;
    use std::sync::mpsc;
    let homes = ShareHomes::new("drop-post");
    let mut ana = Model::open(&homes.0.join("ana")).unwrap().0;
    let mut ben = Model::open(&homes.0.join("ben")).unwrap().0;
    ana.pin_linked(&ben.me(), 3, "Ben").unwrap();
    ben.pin_linked(&ana.me(), 3, "Ana").unwrap();
    ana.sync_start(&ben.me());
    ben.sync_start(&ana.me());
    let town = join(&mut ana, &mut ben, false);
    let (signal, events) = mpsc::sync_channel(1);
    ana.start_store(crate::store_worker::test_pool(), move || { let _ = signal.try_send(()); }).unwrap();
    let own = homes.0.join("pictures");
    std::fs::create_dir_all(&own).unwrap();
    let files = [("shot.png", PNG_1X1.to_vec()), ("lying.png", mp4), ("screen.mov", mov), ("notes.mp4", b"not a movie at all".to_vec())];
    for (name, bytes) in &files {
        std::fs::write(own.join(name), bytes).unwrap();
        ana.post_dropped(town, own.join(name)).unwrap();
    }
    while ana.clips_pending() {
        ana.drain_store();
        if ana.clips_pending() { events.recv().unwrap(); }
    }
    let problems = ana.take_recording_problems();
    assert!(problems.iter().any(|s| s.starts_with("notes.mp4") && s.contains("Only pictures and videos post to the chat.")),
        "a file that is not media gets the one refusal line: {problems:?}");
    assert!(files.iter().all(|(name, _)| own.join(name).is_file()), "a dropped file is never removed, posted or refused");
    let clip = |model: &Model, title: &str| model.state.entities.values()
        .find(|e| e.kind == kind::CLIP && e.text(field::TITLE) == title).cloned();
    let [png, lying, mov] = ["shot.png", "lying.png", "screen.mov"].map(|t| clip(&ana, t).expect(t));
    assert!(clip(&ana, "notes.mp4").is_none(), "a refused file posts nothing");
    for (entity, media) in [(&png, "image/png"), (&lying, "video/mp4"), (&mov, "video/quicktime")] {
        assert_eq!((entity.project, entity.text(field::MEDIA)), (None, media), "{}: no project, kind from the bytes", entity.text(field::TITLE));
    }
    assert_eq!((png.value(field::W), png.value(field::H)), (Some(&Value::Int(1)), Some(&Value::Int(1))), "the picture's size comes from its header");
    assert_eq!(png.value(field::POSTER), png.value(field::BLOB), "a picture is its own poster");
    exchange(&mut ana, &mut ben);
    let view = Workspace::read(&ben, false, &LocalClock::default());
    let rows: Vec<_> = view.rows.iter().filter_map(|r| match r { ChatRow::Clip(c) => Some(c.clone()), _ => None }).collect();
    assert_eq!(rows.len(), 3, "Ben's chat shows each posted clip");
    for (entity, picture) in [(&png, true), (&lying, false), (&mov, false)] {
        let row = rows.iter().find(|c| c.id.1 == entity.id).expect("a row for the clip");
        let media = row.media.as_ref().expect("the row has media");
        assert_eq!((row.title.as_str(), media.is_picture), (entity.text(field::TITLE), picture));
    }
    ana.stop_store();
    while !ana.store_stopped() { ana.drain_store(); if !ana.store_stopped() { events.recv().unwrap(); } }
}

struct Computer {
    model: Model,
    hub: Hub,
    store: SimStore,
    blobs: blobs::BlobPump,
    linked: BTreeSet<Attempt>,
    silent: bool,
    chunk_indices: BTreeSet<u64>,
    wants: Vec<(Id, PublicKey, u64)>,
    held_read: Option<Attempt>,
    address: SocketAddr,
    received_towns: std::collections::BTreeSet<(PublicKey, TownId)>,
    /// Files up to this size are fetched without being opened.
    at_once: u64,
    /// CHUNK payload bytes received from each carrier.
    chunk_bytes: BTreeMap<PublicKey, usize>,
}

impl Computer {
    fn post(&mut self, text: &str) -> Id {
        self.model.act(Action::PostChat { text: text.into(), to_ai: false, reply_to: None }, Origin::Ui).unwrap()
    }

    fn has_file(&self, town: TownId, file: StoredFile) -> bool {
        self.model.home.blobs_dir().join(makepad_town_core::hex(&town))
            .join(format!("{}.tree", makepad_town_core::hex(&file.id))).is_file()
    }

    fn turn(&mut self, net: &mut Net, now: Now) {
        for Output { attempt, event } in self.hub.poll(now, BatchEnd::Drained) {
            let Carrier::Peer(peer) = attempt.carrier else { panic!("remembered carrier") };
            match event {
                Event::Punch => {
                    let mut random = [0; 72];
                    makepad_network::tls::os_random(&mut random).unwrap();
                    self.hub.punch(attempt, random, now);
                }
                Event::Send(to, bytes) => net.send(self.address, to, bytes),
                Event::Linked { peer, .. } => {
                    self.model.link_proved(&peer.device, peer.version, "", None).unwrap();
                    self.linked.insert(attempt);
                    self.blobs.attach(attempt);
                    self.model.attach_blob_view(attempt);
                }
                Event::Sync(msg) => {
                    self.model.receive_from(peer, &msg).unwrap();
                    if self.held_read != Some(attempt) { self.hub.sync_read(attempt); }
                }
                Event::Blob(msg) => {
                    let decoded = blob::Message::decode(&msg).unwrap();
                    self.received_towns.insert((peer, decoded.town));
                    if let Body::Chunk { index, bytes, .. } = decoded.body {
                        self.chunk_indices.insert(index);
                        *self.chunk_bytes.entry(peer).or_default() += bytes.len();
                    }
                    self.blobs.receive(attempt, &msg, now.ms);
                }
                Event::Lost { .. } => {
                    self.blobs.detach(attempt);
                    self.linked.remove(&attempt);
                    self.model.end_carrier(peer, true);
                }
                Event::Failed(why) | Event::Refused(why) => panic!("carrier failed: {why}"),
                _ => {}
            }
        }
        self.model.publish_blob_policy(self.at_once);
        self.store.turn();
        self.blobs.pump(now.ms);
        for &at in &self.linked {
            let Carrier::Peer(peer) = at.carrier else { unreachable!() };
            if self.model.blob_policy_ready(peer) && self.hub.writable(at, Kind::Sync) {
                if let Some(bytes) = self.model.pull_sync(peer) { self.hub.send_sync(at, &bytes); }
            }
            if !self.silent && self.hub.writable(at, Kind::Blob) {
                if let Some(bytes) = self.blobs.poll_send(at) {
                    let msg = blob::Message::decode(&bytes).unwrap();
                    if matches!(msg.body, Body::Want(ranges) if ranges.iter().len() > 0) {
                        self.wants.push((msg.id, peer, now.ms));
                    }
                    self.hub.send_blob(at, &bytes);
                }
            }
        }
    }
}

/// Computers on 192.0.2.x, each with a hub that dials the ones `linked` names.
fn computers(models: Vec<Model>, linked: impl Fn(usize, usize) -> bool) -> (Net, Vec<Computer>) {
    let mut net = Net::new(42, LinkConfig { latency_ms: 1, jitter_ms: 0, loss_per_mille: 0 });
    let addresses: Vec<_> = (1..=models.len() as u8).map(|n| {
        let ip = Ipv4Addr::new(192, 0, 2, n);
        net.add_host(ip, None);
        SocketAddr::from((ip, 4040))
    }).collect();
    let keys: Vec<_> = models.iter().map(Model::me).collect();
    let computers = models.into_iter().enumerate().map(|(i, mut model)| {
        model.sync_stop();
        let mut hub = Hub::new(model.device_key());
        for (j, key) in keys.iter().enumerate().filter(|&(j, _)| j != i && linked(i, j)) {
            model.remember_path(key, PathMemory { addr: Some(addresses[j]), lan: None }).unwrap();
        }
        let devices = model.wanted().0.into_iter().map(|d|
            (d.pin, Remembered { public: d.path.addr, lan: d.path.lan }));
        hub.set_wanted(devices, Now { ms: 0, unix_secs: 0 });
        let mut store = SimStore::open(model.home.clone(), model.open_id(), CHUNK);
        let (blobs, endpoint) = blobs::BlobPump::new(model.me(), CHUNK, || {});
        model.bind_blobs(endpoint, store.attach());
        Computer { model, hub, store, blobs, linked: BTreeSet::new(), silent: false, chunk_indices: BTreeSet::new(), wants: Vec::new(), held_read: None, address: addresses[i],
            received_towns: Default::default(), at_once: u64::MAX, chunk_bytes: BTreeMap::new() }
    }).collect();
    (net, computers)
}

fn turn_all(net: &mut Net, computers: &mut [Computer]) {
    let ms = net.now() + 5;
    let now = Now { ms, unix_secs: ms / 1000 };
    for c in computers.iter_mut() { c.hub.begin_batch(now); }
    for packet in net.advance_to(ms) {
        let c = computers.iter_mut().find(|c| c.address == packet.to).unwrap();
        c.hub.handle(packet.from, &packet.bytes, ms);
    }
    for c in computers.iter_mut() { c.turn(net, now); }
}

struct TwoTowns { computers: [Computer; 3], net: Net }

impl TwoTowns {
    fn new(models: [Model; 3]) -> Self {
        let (net, computers) = computers(models.into(), |i, j| i == 0 || j == 0);
        let Ok(computers) = computers.try_into() else { unreachable!() };
        Self { net, computers }
    }

    fn turn(&mut self) { turn_all(&mut self.net, &mut self.computers); }

    fn until(&mut self, what: &str, mut reached: impl FnMut(&mut [Computer; 3]) -> bool) -> usize {
        for turns in 0..4000 {
            if reached(&mut self.computers) { return turns }
            self.turn();
        }
        panic!("simulated turns exhausted waiting for {what}");
    }
}

#[test]
fn two_towns_sync_and_transfer_files_on_independent_carriers() {
    let (homes, mut ana, ben) = shared_town("two-towns");
    let ben_town = ana.open_id();
    ana.sync_stop();
    let mut kit = Model::open(&homes.0.join("kit")).unwrap().0;
    ana.pin_linked(&kit.me(), 3, "Kit").unwrap();
    kit.pin_linked(&ana.me(), 3, "Ana").unwrap();
    ana.sync_start(&kit.me());
    kit.sync_start(&ana.me());
    let kit_town = crate::model::tests::join(&mut kit, &mut ana, false);
    let mut m = TwoTowns::new([ana, ben, kit]);
    m.until("both proved sessions on Ana", |[a, b, k]| a.linked.len() == 2 && b.linked.len() == 1 && k.linked.len() == 1);
    for _ in 0..64 { m.turn(); }
    let marker = |m: &mut TwoTowns, text| {
        let id = m.computers[2].post(text);
        m.until("Kit's line in Ana's fold", |[a, _, _]| a.model.event(&id).is_some())
    };
    let quiet = marker(&mut m, "first marker");
    for _ in 0..64 { m.turn(); }
    let text = "x".repeat(16 * 1024);
    let ben = m.computers[0].hub.attempt(Carrier::Peer(m.computers[1].model.me())).unwrap();
    m.computers[0].held_read = Some(ben);
    let ben_last = (0..10).map(|_| m.computers[1].post(&text)).last().unwrap();
    m.turn();
    let busy = marker(&mut m, "other marker");
    assert_eq!(busy, quiet, "Kit's chat takes the same number of turns while Ben is syncing");
    m.until("Ana holds a message from Ben", |[a, _, _]| a.hub.awaiting_sync());
    let stalled = marker(&mut m, "one more from Kit");
    assert_eq!(stalled, quiet, "Kit's line reaches Ana's fold while Ben waits for the UI");
    eprintln!("Kit's chat reached Ana in {quiet} quiet, {busy} busy and {stalled} stalled turns");
    m.computers[0].model.open_town(ben_town).unwrap();
    assert!(m.computers[0].model.event(&ben_last).is_none(), "Ben still has history in flight");
    m.computers[0].held_read = None;
    m.computers[0].hub.sync_read(ben);
    m.until("Ben's backlog in Ana's fold", |[a, _, _]| a.model.event(&ben_last).is_some());
    let ana_line = m.computers[0].post("For Ben");
    m.until("Ana's reply in Ben's fold", |[_, b, _]| b.model.event(&ana_line).is_some());
    assert!(m.computers[2].model.event(&ana_line).is_none());
    m.computers[0].model.open_town(kit_town).unwrap();
    let ana_line = m.computers[0].post("For Kit");
    m.until("Ana's reply in Kit's fold", |[_, _, k]| k.model.event(&ana_line).is_some());
    assert!(m.computers[1].model.event(&ana_line).is_none());

    for _ in 0..64 { m.turn(); }
    let [a, b, k] = &mut m.computers;
    let peers = [b.model.me(), k.model.me()];
    let mut idle = Vec::new();
    for _ in 0..129 {
        let start = Instant::now();
        assert!(a.model.publish_blob_policy(u64::MAX));
        for peer in peers { assert!(a.model.pull_sync(peer).is_none()); }
        idle.push(start.elapsed());
    }
    idle.sort_unstable();
    eprintln!("two carriers, two towns, idle blob publication and sync pulls: median {:?}, p95 {:?}", idle[64], idle[122]);

    let ben_file = add(&mut b.model, &mut b.store, b"Ben");
    let kit_file = add(&mut k.model, &mut k.store, &[7; CHUNK * 64]);
    // Two opened files can transfer together even though automatic downloads share a single slot.
    m.until("Ana can open both signed files", |[a, _, _]|
        a.model.want_file(ben_town, ben_file.id).is_ok() && a.model.want_file(kit_town, kit_file.id).is_ok());
    m.until("Ben's file and a verified prefix from Kit", |[a, _, _]| a.has_file(ben_town, ben_file)
        && matches!(a.model.file_transfer(kit_town, kit_file.id), Some(blob::lane::Status::Progress { have, .. }) if *have > 0));
    let [a, b, k] = &mut m.computers;
    // Kit knows the other file's exact town and hash. Its authenticated carrier
    // may ask, but Ana must send it no traffic for Ben's town.
    let ka = k.hub.attempt(Carrier::Peer(a.model.me())).unwrap();
    let want = blob::Message { town: ben_town, id: ben_file.id, body: Body::Want(blob::Ranges::Local(&[0..1])) };
    k.hub.send_blob(ka, &want.encode().unwrap());
    assert!(!a.has_file(kit_town, kit_file));
    assert!(!b.has_file(kit_town, kit_file) && !k.has_file(ben_town, ben_file));
    let now = Now { ms: m.net.now(), unix_secs: m.net.now() / 1000 };
    a.hub.set_wanted([(Pin { device: k.model.me(), min_version: makepad_town_core::link::VERSION },
        Remembered { public: Some(k.address), lan: None })], now);
    b.hub.set_wanted([], now);
    m.until("Ben detached and Kit's file complete", |[a, b, _]| a.linked.len() == 1 && b.linked.is_empty() && a.has_file(kit_town, kit_file));
    let [a, _, k] = &m.computers;
    assert!(a.received_towns.contains(&(k.model.me(), ben_town)), "the unauthorized WANT arrived");
    assert!(!k.received_towns.contains(&(a.model.me(), ben_town)), "Kit heard nothing for Ben's town");
    for (town, file, expected) in [(ben_town, ben_file, b"Ben".as_slice()), (kit_town, kit_file, &[7; CHUNK * 64])] {
        let path = a.model.home.blobs_dir().join(makepad_town_core::hex(&town)).join(makepad_town_core::hex(&file.id));
        assert_eq!(std::fs::read(path).unwrap(), expected);
    }
}

#[test]
fn a_replaced_blob_attempt_cannot_publish_its_view_or_spend_the_new_write_credit() {
    let (_homes, ana, ben) = shared_town("blob-attempt");
    let town = ana.open_id();
    let mut store = SimStore::open(ana.home.clone(), town, CHUNK);
    let data = b"abcdef";
    let tree = blob::Tree::new(data, CHUNK).unwrap();
    let size = data.len() as u64;
    let id = blob::id(size, &tree.root());
    let (mut pump, endpoint) = blobs::BlobPump::new(ana.me(), CHUNK, || {});
    let mut at = attempt(ben.me());
    let publish = |at, port| {
        let selection = blobs::Selection { refs: [(id, (size, tree.root()))].into(), authored: Default::default(),
            small: BTreeSet::new() };
        let view = blobs::View { towns: [(town, Arc::new(selection))].into(), ..Default::default() };
        assert!(endpoint.to_link.try_send(blobs::Update { views: [(at, view)].into(),
            explicit: Arc::new([(town,id)].into()), store: port }).is_ok());
    };
    let send_chunk = |pump: &mut blobs::BlobPump, at, index: u64, bytes: &[u8]| {
        let proof = tree.proof(index).unwrap();
        let wire = blob::Message { town, id, body: Body::Chunk { index, proof: &proof, bytes } }.encode().unwrap();
        pump.receive(at, &wire, 0);
        pump.pump(0); // The write is now on the one shared store port.
    };
    pump.attach(at);
    publish(at, Some(store.attach()));
    for (index, bytes) in [(0, &data[..3]), (1, &b"bad"[..]), (1, &data[3..])] {
        let mut grant = Vec::new();
        for _ in 0..16 {
            store.turn();
            pump.pump(0);
            while let Some(wire) = pump.poll_send(at) {
                if let Body::Want(r) = blob::Message::decode(&wire).unwrap().body { grant.extend(r.iter()); }
            }
            if !grant.is_empty() { break; }
        }
        assert_eq!(grant, [index..2], "the disk prefix survives; an old bad proof cannot blacklist the replacement");
        send_chunk(&mut pump, at, index, bytes);
        if bytes == &data[3..] { break; }
        publish(at, None); // The UI had already queued this snapshot on the old attempt.
        pump.detach(at);
        at.generation += 1;
        pump.attach(at);
        pump.pump(0);
        assert!(pump.poll_send(at).is_none(), "the retired view cannot announce READY on the replacement");
        publish(at, None);
        pump.pump(0); // Authorize the replacement before delivering the old disk reply.
    }
    for _ in 0..8 { store.turn(); pump.pump(0); }
    assert!(store.absent(town,id).is_empty());
}

/// Kit opens a file that every one of its carriers holds. Each chunk crosses one carrier.
#[test]
fn an_opened_file_held_by_several_carriers_is_pulled_once() {
    use crate::model::tests::group_town;
    let mut report = Vec::new();
    let mut once = true;
    for holders in [1, 2, 4] {
        let (_homes, mut models) = group_town("many-holders", holders + 1);
        let kit = holders;
        let kit_key = models[kit].me();
        for i in 0..holders {
            let key = models[i].me();
            models[i].pin_linked(&kit_key, 3, "Kit").unwrap();
            models[kit].pin_linked(&key, 3, &format!("Holder {i}")).unwrap();
        }
        let town = models[0].open_id();
        let (mut net, mut c) = computers(models, |i, j| i == kit || j == kit);
        c[kit].at_once = 0;
        let until = |net: &mut Net, c: &mut Vec<Computer>, what: &str, reached: &dyn Fn(&[Computer]) -> bool| {
            for turns in 0..4000 {
                if reached(c) { return turns }
                turn_all(net, c);
            }
            panic!("simulated turns exhausted waiting for {what}");
        };
        until(&mut net, &mut c, "a carrier to every holder", &|c| c[kit].linked.len() == holders);
        let bytes: Vec<u8> = (0..CHUNK * 600).map(|i| i as u8).collect();
        let file = c.iter_mut().take(holders).map(|h| h.store.add(town, &bytes)).last().unwrap();
        let reference = file_reference(&mut c[0].model, file);
        until(&mut net, &mut c, "the reference on every computer", &|c| c.iter().all(|m| m.model.event(&reference).is_some()));
        for _ in 0..64 { turn_all(&mut net, &mut c); }
        assert!(c.iter().take(holders).all(|h| h.has_file(town, file)), "every holder has the whole file");
        c[kit].chunk_bytes.clear();
        c[kit].chunk_indices.clear();
        c[kit].model.want_file(town, file.id).unwrap();
        let turns = until(&mut net, &mut c, "Kit's copy", &|c| c[kit].has_file(town, file));
        for _ in 0..64 { turn_all(&mut net, &mut c); }
        let pulled: usize = c[kit].chunk_bytes.values().sum();
        once &= pulled == bytes.len() && c[kit].chunk_indices.len() == 600 && c[kit].chunk_bytes.len() == 1;
        assert_eq!(c[kit].chunk_bytes.keys().next(), Some(&c[0].model.me()), "the entity's author holds the file");
        report.push(format!("{holders} holders: {pulled} B, {} indices, {} sender, {turns} turns",
            c[kit].chunk_indices.len(), c[kit].chunk_bytes.len()));

        if holders == 2 {
            let author = c[0].model.me();
            let alternate = c[1].model.me();
            for cut in [false, true] {
                let content = vec![if cut { 11 } else { 22 }; CHUNK * 80];
                let file = c[0].store.add(town, &content);
                c[1].store.add(town, &content);
                let reference = file_reference(&mut c[0].model, file);
                until(&mut net, &mut c, "the next signed reference", &|c| c.iter().all(|m| m.model.event(&reference).is_some()));
                for _ in 0..64 { turn_all(&mut net, &mut c); }
                c[kit].chunk_bytes.clear();
                c[kit].chunk_indices.clear();
                c[0].silent = !cut;
                c[kit].model.want_file(town, file.id).unwrap();
                let lost_at = if cut {
                    until(&mut net, &mut c, "a verified prefix from the author", &|c|
                        matches!(c[kit].model.file_transfer(town, file.id), Some(blob::lane::Status::Progress { have, .. }) if *have > 0));
                    let now = Now { ms: net.now(), unix_secs: net.now() / 1000 };
                    let path = Remembered { public: Some(c[1].address), lan: None };
                    c[kit].hub.set_wanted([(Pin { device: alternate, min_version: makepad_town_core::link::VERSION }, path)], now);
                    c[0].hub.set_wanted([], now);
                    Some(now.ms)
                } else { None };
                until(&mut net, &mut c, "the complete file from the remaining holder", &|c| c[kit].has_file(town, file));
                for _ in 0..64 { turn_all(&mut net, &mut c); }
                let mut requests = c[kit].wants.iter().filter(|(id, _, _)| *id == file.id);
                let &(_, first, start) = requests.next().unwrap();
                assert_eq!(first, author);
                let &(_, _, switched) = requests.find(|(_, peer, _)| *peer == alternate).unwrap();
                assert_eq!(c[kit].chunk_indices.len(), 80);
                let total = c[kit].chunk_bytes.values().sum::<usize>();
                if let Some(lost) = lost_at {
                    assert!(switched >= lost && switched - lost < 4_000, "slot loss reselects before the silence deadline");
                    assert_eq!(c[kit].chunk_bytes.len(), 2);
                    assert!(total <= content.len() + blob::lane::MAX_WINDOW * CHUNK);
                } else {
                    assert_eq!(switched - start, 4_000, "an authenticated but silent holder yields at its deadline");
                    assert_eq!(c[kit].chunk_bytes, BTreeMap::from([(alternate, content.len())]));
                }
                c[0].silent = false;
            }
        }
    }
    eprintln!("{}", report.join("; "));
    assert!(once, "each chunk crosses one carrier: {}", report.join("; "));
}
