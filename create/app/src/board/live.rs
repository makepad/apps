//! Ephemeral board previews, in world coordinates. Only the normal gesture release edits the log.
use super::gesture::{FixedRect, Preview, Transform};
use super::tree;
use crate::call::topic::BOARD_LIVE;
use crate::tweak::{overlay, Relay, Seat};
use makepad_widgets::makepad_platform::monotonic_seconds;
use makepad_town_core::{entity::{field, kind}, event::Id, fold::State};
use makepad_widgets::{dvec2, log, Cx, DVec2, Timer, Vec4f};
use std::collections::HashMap;

const SILENCE: f64 = 1.5;

#[derive(PartialEq)]
pub(super) struct Motion {
    pub ids: Vec<Id>,
    pub transform: Transform,
    pub pointer: DVec2,
}

enum Message {
    Moving(Motion),
    End { dropped: bool },
}

// Format 1 uses little-endian integers and IEEE-754 doubles, with i128 endpoints just like
// the gesture (the far edge of an i64 position and size need not fit an i64).
fn encode(board: Id, motion: Option<&Motion>, dropped: bool) -> Vec<u8> {
    // Beyond the wire count, keep the cursor live. The signed drop still carries the full edit.
    let n = motion.and_then(|m| u16::try_from(m.ids.len()).ok()).unwrap_or(0);
    let length = motion.map_or(35, |m| 53 + usize::from(n) * 32 + match m.transform {
        Transform::Move { .. } => 16, Transform::Resize { .. } => 128,
    });
    let mut bytes = Vec::with_capacity(length);
    bytes.extend([1, if motion.is_some() { 1 } else { 3 }]);
    bytes.extend(board);
    if let Some(m) = motion {
        bytes.extend(n.to_le_bytes());
        for id in &m.ids[..usize::from(n)] { bytes.extend(id); }
        match m.transform {
            Transform::Move { dx, dy } => {
                bytes.push(1);
                bytes.extend(dx.to_le_bytes());
                bytes.extend(dy.to_le_bytes());
            }
            Transform::Resize { from, to } => {
                bytes.push(2);
                for r in [from, to] {
                    for n in r.lo.into_iter().chain(r.hi) { bytes.extend(n.to_le_bytes()); }
                }
            }
        }
        bytes.extend(m.pointer.x.to_le_bytes());
        bytes.extend(m.pointer.y.to_le_bytes());
    } else {
        bytes.push(u8::from(dropped));
    }
    bytes
}

fn take<const N: usize>(bytes: &mut &[u8]) -> Option<[u8; N]> {
    let (value, rest) = bytes.split_at_checked(N)?;
    *bytes = rest;
    value.try_into().ok()
}

fn fixed_rect(bytes: &mut &[u8]) -> Option<FixedRect> {
    let mut pair = || Some([i128::from_le_bytes(take(bytes)?), i128::from_le_bytes(take(bytes)?)]);
    let r = FixedRect { lo: pair()?, hi: pair()? };
    // A captured edge is the sum of at most two i64 fields. A resized edge can extend it by
    // at most the gesture's maximum span (2 * i64::MAX - i64::MIN).
    let bound = 5 * -i128::from(i64::MIN);
    (0..2).all(|i| -bound <= r.lo[i] && r.lo[i] <= r.hi[i] && r.hi[i] <= bound).then_some(r)
}

fn decode(bytes: &[u8]) -> Option<(Id, Message)> {
    let [1, kind, mut_body @ ..] = bytes else { return None };
    let mut body = mut_body;
    let board = take(&mut body)?;
    let message = match kind {
        1 => {
            let count = usize::from(u16::from_le_bytes(take(&mut body)?));
            let (ids, rest) = body.split_at_checked(count * 32)?;
            body = rest;
            let transform = match take(&mut body)? {
                [1] => Transform::Move { dx: i64::from_le_bytes(take(&mut body)?), dy: i64::from_le_bytes(take(&mut body)?) },
                [2] => Transform::Resize { from: fixed_rect(&mut body)?, to: fixed_rect(&mut body)? },
                _ => return None,
            };
            let pointer = dvec2(f64::from_le_bytes(take(&mut body)?), f64::from_le_bytes(take(&mut body)?));
            if !pointer.x.is_finite() || !pointer.y.is_finite() || !body.is_empty() { return None }
            Message::Moving(Motion { ids: ids.chunks_exact(32).map(|id| id.try_into().unwrap()).collect(), transform, pointer })
        }
        3 => Message::End { dropped: match body { [0] => false, [1] => true, _ => return None } },
        _ => return None,
    };
    Some((board, message))
}

#[derive(Default)]
struct Cost {
    seconds: f64,
    samples: usize,
}

impl Cost {
    fn sample(&mut self, start: f64) {
        self.seconds += monotonic_seconds() - start;
        self.samples += 1;
    }

    fn mean_us(&self) -> f64 { self.seconds * 1e6 / self.samples.max(1) as f64 }
}

struct Drag {
    town: Option<Id>,
    motion: Motion,
    end: Option<bool>,
    dirty: bool,
    send: Cost,
}

impl Drag {
    fn send_frame(&mut self, board: Id, model: &crate::model::Model, relay: &mut impl Relay) {
        let start = monotonic_seconds();
        let town = *self.town.get_or_insert_with(|| model.open_id());
        let Some((state, _)) = model.tweak_board_state(town) else { return };
        if !board_exists(state, &board) || !state.active_member(&model.me()) { return }
        let mut members = relay.peers().into_iter().filter(|seat| state.active_member(seat));
        let Some(first) = members.next() else { return };
        let body = encode(board, self.end.is_none().then_some(&self.motion), self.end.unwrap_or(false));
        for seat in members { let _ = relay.send_latest(seat, BOARD_LIVE, body.clone()); }
        let _ = relay.send_latest(first, BOARD_LIVE, body);
        self.send.sample(start);
    }
}

#[derive(Default)]
pub(super) struct Outbox {
    boards: HashMap<Id, Drag>,
    heartbeat: Timer,
}

impl Outbox {
    pub fn moving(&mut self, cx: &mut Cx, board: Id, preview: &Preview, pointer: DVec2) -> bool {
        if let Some(drag) = self.boards.get_mut(&board).filter(|d| d.end.is_none()) {
            let m = &mut drag.motion;
            // The captured set can only shrink (an item was locked or removed mid-drag).
            let ids_changed = m.ids.len() != preview.items.len();
            if !ids_changed && m.transform == preview.transform && m.pointer == pointer { return false }
            if ids_changed { m.ids = preview.items.keys().copied().collect(); }
            m.transform = preview.transform;
            m.pointer = pointer;
            drag.dirty = true;
            return true
        }
        self.boards.insert(board, Drag {
            town: None, motion: Motion { ids: preview.items.keys().copied().collect(), transform: preview.transform, pointer },
            end: None, dirty: true, send: Cost::default(),
        });
        cx.stop_timer(std::mem::take(&mut self.heartbeat));
        self.heartbeat = cx.start_interval(0.5);
        true
    }

    pub fn beat(&mut self, event: &makepad_widgets::Event, board: Option<Id>) -> bool {
        if self.heartbeat.is_event(event).is_none() { return false }
        let Some(drag) = board.and_then(|id| self.boards.get_mut(&id)).filter(|d| d.end.is_none()) else { return false };
        drag.dirty = true;
        true
    }

    pub fn end(&mut self, cx: &mut Cx, board: Id, dropped: bool) -> bool {
        let Some(drag) = self.boards.get_mut(&board) else { return false };
        if drag.end.is_some() { return false }
        cx.stop_timer(std::mem::take(&mut self.heartbeat));
        drag.end = Some(dropped);
        drag.dirty = true;
        true
    }

    pub fn frame(&mut self, model: &crate::model::Model, mut relay: Option<&mut impl Relay>) {
        // On a board switch, queue the departed board's cancel before the new board's preview.
        for ending in [true, false] {
            self.boards.retain(|board, drag| {
                if drag.end.is_some() != ending || !std::mem::take(&mut drag.dirty) { return true }
                if let Some(link) = relay.as_deref_mut() { drag.send_frame(*board, model, link); }
                if ending {
                    let detail = if drag.motion.ids.len() > usize::from(u16::MAX) { ", cursor only" } else { "" };
                    log!("board live send: {} frames, mean encode-and-send {:.1} us/frame ({} items{})",
                        drag.send.samples, drag.send.mean_us(), drag.motion.ids.len(), detail);
                }
                !ending
            });
        }
    }
}

fn board_exists(state: &State, board: &Id) -> bool {
    state.entities.get(board).is_some_and(|e| e.kind == kind::BOARD && tree::live(e))
}

// The call delivers folded batches, not individual event ids. Remember only the newest
// geometry writer from this mover for each dragged item. A chat or another mover cannot end it.
fn last_write(state: &State, item: &Id, seat: &Seat) -> Option<(u64, Id)> {
    let e = state.entities.get(item)?;
    [field::X, field::Y, field::W, field::H].into_iter().filter_map(|field| {
        let (height, author, id) = state.order_of(&e.fields.get(&field)?.writer)?;
        (author == *seat).then_some((height, id))
    }).max()
}

pub(super) struct Ghost {
    pub motion: Motion,
    pub colour: Vec4f,
    pub name: String,
    writes: Vec<Option<(u64, Id)>>,
    last: f64,
    dropped: bool,
    receive: Cost,
    draw: Cost,
}

impl Ghost {
    pub fn drew(&mut self, start: f64) { self.draw.sample(start); }

    fn report(&self) {
        log!("board live receive: {} updates, {} draws, mean receive-and-draw {:.1} us/frame (receive {:.1} us/update, draw {:.1} us/frame, {} items)",
            self.receive.samples, self.draw.samples, (self.receive.seconds + self.draw.seconds) * 1e6 / self.draw.samples.max(1) as f64,
            self.receive.mean_us(), self.draw.mean_us(), self.motion.ids.len());
    }
}

#[derive(Default)]
pub(super) struct Incoming {
    pub ghosts: HashMap<(Seat, Id), Ghost>,
    // A signed edit can overtake motion on the independent peer topic. Consume the tail
    // of that gesture through its End, with the same silence deadline if the link disappears.
    awaiting_end: HashMap<(Seat, Id), f64>,
    timer: Timer,
}

impl Incoming {
    pub fn receive(&mut self, cx: &mut Cx, state: &State, seat: Seat, bytes: &[u8], visible: Option<Id>) -> bool {
        let start = monotonic_seconds();
        if !state.active_member(&seat) { return false }
        let Some((board, msg)) = decode(bytes) else { return false };
        if !board_exists(state, &board) { return false }
        let key = (seat, board);
        if self.awaiting_end.contains_key(&key) {
            if matches!(msg, Message::End { .. }) { self.awaiting_end.remove(&key); }
            return false
        }
        match msg {
            Message::End { dropped } => {
                let Some(g) = self.ghosts.get_mut(&key) else { return false };
                g.receive.sample(start);
                if dropped {
                    g.last = start;
                    g.dropped = true;
                    return false
                }
                self.ghosts.remove(&key).unwrap().report();
            }
            Message::Moving(motion) => {
                if let Some(g) = self.ghosts.get_mut(&key).filter(|g| !g.dropped) {
                    g.last = start;
                    if g.motion.ids != motion.ids {
                        g.writes = motion.ids.iter().map(|id| last_write(state, id, &seat)).collect();
                    } else if g.motion.transform == motion.transform && g.motion.pointer == motion.pointer {
                        g.receive.sample(start);
                        return false;
                    }
                    g.motion = motion;
                    g.receive.sample(start);
                } else {
                    let writes = motion.ids.iter().map(|id| last_write(state, id, &seat)).collect();
                    let mut ghost = Ghost { motion, writes, last: start, dropped: false,
                        name: overlay::who(state, &seat, None), colour: overlay::seat_colour(state, &seat),
                        receive: Cost::default(), draw: Cost::default() };
                    ghost.receive.sample(start);
                    if let Some(previous) = self.ghosts.insert(key, ghost) { previous.report(); }
                }
                if self.timer.is_empty() { self.timer = cx.start_timeout(SILENCE); }
            }
        }
        Some(board) == visible
    }

    pub fn reconcile(&mut self, state: &State, visible: Option<Id>) -> bool {
        let mut redraw = false;
        self.ghosts.retain(|(seat, board), g| {
            if state.active_member(seat) && board_exists(state, board) {
                let committed = g.motion.ids.iter().zip(&g.writes).any(|(id, before)| last_write(state, id, seat) > *before);
                if !committed { return true }
                if !g.dropped { self.awaiting_end.insert((*seat, *board), monotonic_seconds()); }
            }
            g.report();
            redraw |= Some(*board) == visible;
            false
        });
        redraw
    }

    pub fn expire(&mut self, cx: &mut Cx, event: &makepad_widgets::Event, visible: Option<Id>) -> bool {
        if self.timer.is_event(event).is_none() { return false }
        self.timer = Timer::empty();
        let time = monotonic_seconds();
        let mut redraw = false;
        let mut next = SILENCE;
        self.ghosts.retain(|(_, board), g| {
            let left = g.last + SILENCE - time;
            if left <= 0. { g.report(); redraw |= Some(*board) == visible; }
            else { next = next.min(left); }
            left > 0.
        });
        self.awaiting_end.retain(|_, last| {
            let left = *last + SILENCE - time;
            if left > 0. { next = next.min(left); }
            left > 0.
        });
        if !self.ghosts.is_empty() || !self.awaiting_end.is_empty() { self.timer = cx.start_timeout(next); }
        redraw
    }
}

/// Drain authenticated board traffic and flush dirty outboxes on their requested display frame.
pub fn poll(cx: &mut Cx, event: &makepad_widgets::Event, ui: &makepad_widgets::WidgetRef,
    model: Option<&crate::model::Model>, mut relay: Option<&mut impl Relay>) {
    use makepad_widgets::*;
    let messages = relay.as_mut().map_or_else(Vec::new, |r| r.take(BOARD_LIVE));
    if messages.is_empty() && !matches!(event, Event::NextFrame(_) | Event::Timer(_)) { return }
    let Some(model) = model else { return };
    let surface = ui.widget(cx, ids!(canvas.surface));
    if let Some(mut canvas) = surface.borrow_mut::<super::canvas::BoardCanvas>() {
        canvas.poll_live(cx, event, model, relay, messages);
    };
}

#[test]
fn board_drag_coalesces_on_the_frame_and_link_then_signs_one_drop() {
    use super::{gesture::Gesture, BoardView};
    use crate::{call::{CallController, CallEnv, NotLinked}, model::{Action, Model, Origin}};
    use makepad_town_core::event::Key;
    use std::sync::Arc;

    // Count calls at the relay boundary as well as whole messages at the far end of the link.
    struct Count<'a> { peer: Seat, call: &'a mut CallController, sends: usize }
    impl Relay for Count<'_> {
        fn peers(&self) -> Vec<Seat> { vec![[0; 65], self.peer] }
        fn send(&mut self, _: Seat, _: u8, _: Vec<u8>) -> Result<(), NotLinked> {
            panic!("a preview must use the latest-only transport")
        }
        fn send_latest(&mut self, peer: Seat, topic: u8, body: Vec<u8>) -> Result<(), NotLinked> {
            assert_eq!(peer, self.peer, "only town members receive the board preview");
            self.sends += 1;
            self.call.send_latest(peer, topic, body)
        }
        fn take(&mut self, topic: u8) -> Vec<(Seat, Vec<u8>)> { self.call.take(topic) }
    }

    let dir = std::env::temp_dir().join(format!("town-board-live-{}", std::process::id()));
    let (mut model, _) = Model::open(&dir).unwrap();
    let board = model.act(Action::AddBoard { title: "Pair".into(), project: model.project().id }, Origin::Ui).unwrap();
    let item = model.act(Action::AddItem { board, parent: None, kind: Key::Number(kind::STICKY),
        x: 0, y: 0, w: 1024, h: 1024, style: Default::default() }, Origin::Ui).unwrap();
    let seat = model.me();
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut call = CallController::default();
    call.configure(CallEnv { files: Default::default(), devices_path: Default::default(), voice: makepad_call::driver::VoiceSource::Off,
        spawn: Arc::new(|_, _| panic!("a board drag needs no call worker")),
        jobs: Arc::new(|_, _| panic!("a board drag needs no audio job")), signal: Arc::new(|| {}) });
    let port = call.link_up(&mut cx, [4; 65], seat, "Mover").unwrap();
    let mut relay = Count { peer: seat, call: &mut call, sends: 0 };
    let deliver = |relay: &mut Count, cx: &mut Cx| {
        relay.call.poll(cx);
        for frame in port.messages_out.try_iter() { port.messages_in.try_send(frame).unwrap(); }
        relay.call.poll(cx);
        relay.take(BOARD_LIVE)
    };

    let before = model.open_event_count();
    let view = BoardView::build(&model.state, board);
    let mut gesture = Gesture::default();
    let mut out = Outbox::default();
    let mut incoming = Incoming::default();
    gesture.press(&view, Some(item), dvec2(0., 0.), dvec2(0., 0.), false, None, false);
    let mut motion = |x, gesture: &mut Gesture, out: &mut Outbox| {
        let p = dvec2(x, -3.);
        gesture.motion(&view, p, p, 1., true, false);
        out.moving(&mut cx, board, gesture.moving().unwrap(), p);
    };
    for x in [4., 5., 6.] { motion(x, &mut gesture, &mut out); }
    assert_eq!(relay.sends, 0, "input within a frame only replaces the board outbox");
    out.frame(&model, Some(&mut relay));
    out.frame(&model, Some(&mut relay));
    assert_eq!(relay.sends, 1, "three moves in one frame and a still frame produce one send");
    motion(9., &mut gesture, &mut out);
    out.frame(&model, Some(&mut relay));
    assert_eq!(relay.sends, 2, "a change on the next frame reaches the same queued link");
    let received = deliver(&mut relay, &mut cx);
    assert_eq!(received.len(), 1, "the second frame replaced the first unsent body on the real link");
    let (sender, body) = &received[0];
    let expected = Motion { ids: vec![item], transform: Transform::Move { dx: 9*64, dy: -3*64 }, pointer: dvec2(9., -3.) };
    assert!(matches!(decode(body), Some((id, Message::Moving(m))) if id == board && m == expected),
        "the codec round-trips the board, item, signed ticks and world pointer of the latest move");
    assert!(incoming.receive(&mut cx, &model.state, *sender, body, Some(board)), "a move invalidates the visible overlay");
    assert_eq!(incoming.ghosts[&(seat, board)].motion.transform.item_rect(view.get(&item).unwrap()),
        gesture.moving().unwrap().rect(&item).unwrap(), "the remote outline follows the local preview exactly");
    let beat = makepad_widgets::Event::Timer(makepad_widgets::TimerEvent { timer_id: out.heartbeat.0, time: None });
    assert!(!out.heartbeat.is_empty() && out.beat(&beat, Some(board)), "a held drag's heartbeat requests a live frame");
    out.frame(&model, Some(&mut relay));
    out.frame(&model, Some(&mut relay));
    assert_eq!(relay.sends, 3, "one heartbeat re-sends once across two frames without pointer input");
    assert_eq!(deliver(&mut relay, &mut cx), received, "the held preview reaches the peer with the same geometry and pointer");
    incoming.ghosts.get_mut(&(seat, board)).unwrap().last -= SILENCE;
    assert!(!incoming.receive(&mut cx, &model.state, seat, body, Some(board)), "a heartbeat keeps the settled overlay intact");
    let expiry = makepad_widgets::Event::Timer(makepad_widgets::TimerEvent { timer_id: incoming.timer.0, time: None });
    assert!(!incoming.expire(&mut cx, &expiry, Some(board)) && incoming.ghosts.contains_key(&(seat, board)),
        "the heartbeat extends the ghost beyond its original silence deadline");
    assert_eq!(model.open_event_count(), before, "preview frames never append an edit");
    let mut log_first = Incoming::default();
    log_first.receive(&mut cx, &model.state, seat, body, Some(board));
    let action = gesture.release(&view).expect("dropping the preview produces its normal edit");
    model.act(action, Origin::Ui).unwrap();
    out.end(&mut cx, board, true);
    assert!(out.heartbeat.is_empty() && !out.beat(&beat, Some(board)), "ending the drag stops its timer and ignores a late beat");
    out.frame(&model, Some(&mut relay));
    out.frame(&model, Some(&mut relay));
    let end = deliver(&mut relay, &mut cx);
    assert_eq!(relay.sends, 4, "the end is the last send from the held drag");
    assert!(matches!(end.as_slice(), [(s, bytes)] if *s == seat &&
        matches!(decode(bytes), Some((id, Message::End { dropped: true })) if id == board)), "the drop sends one end");
    assert_eq!(model.open_event_count(), before + 1, "the drop signs exactly one log edit");
    incoming.receive(&mut cx, &model.state, seat, &end[0].1, Some(board));
    assert!(incoming.ghosts.contains_key(&(seat, board)), "a dropped end keeps the ghost until the signed edit arrives");
    assert!(incoming.reconcile(&model.state, Some(board)) && incoming.ghosts.is_empty(),
        "the mover's signed edit clears the ghost and invalidates its overlay");
    assert!(log_first.reconcile(&model.state, Some(board)), "the signed edit can arrive before its end");
    assert!(!log_first.receive(&mut cx, &model.state, seat, body, Some(board)) && log_first.ghosts.is_empty(),
        "a late preview cannot move the already committed item a second time");
    log_first.receive(&mut cx, &model.state, seat, &end[0].1, Some(board));
    assert!(log_first.receive(&mut cx, &model.state, seat, body, Some(board)), "the next drag can start after the late end");

    let wide = i128::from(i64::MAX);
    let resize = Motion { ids: vec![item], pointer: dvec2(3.5, -7.25), transform: Transform::Resize {
        from: FixedRect { lo: [-wide, 0], hi: [wide + 10, 512] },
        to: FixedRect { lo: [-wide, -64], hi: [wide + 20, 1024] },
    } };
    let mut bytes = encode(board, Some(&resize), false);
    assert!(matches!(decode(&bytes), Some((_, Message::Moving(m))) if m == resize),
        "resize preserves wide fixed-point endpoints and a fractional pointer");
    bytes[1] = 2;
    assert!(decode(&bytes).is_none(), "reserved freehand messages are ignored");
    bytes[0] = 0;
    bytes[1] = 1;
    assert!(decode(&bytes).is_none(), "an unknown format cannot create a ghost");

    incoming.receive(&mut cx, &model.state, seat, body, Some(board));
    assert!(incoming.receive(&mut cx, &model.state, seat, &encode(board, None, false), Some(board)) && incoming.ghosts.is_empty(),
        "cancel immediately erases the ghost without needing a signed edit");
    incoming.receive(&mut cx, &model.state, seat, body, Some(board));
    let timer = makepad_widgets::Event::Timer(makepad_widgets::TimerEvent { timer_id: incoming.timer.0, time: None });
    incoming.ghosts.get_mut(&(seat, board)).unwrap().last -= SILENCE;
    assert!(incoming.expire(&mut cx, &timer, Some(board)) && incoming.ghosts.is_empty(),
        "1.5 seconds of silence erases the ghost without needing another packet");
    drop(model);
    std::fs::remove_dir_all(dir).unwrap();
}
