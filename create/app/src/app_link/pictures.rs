use super::{AppLinkEnv, WindowId};
use makepad_call::driver::codec::{Codecs, Decoder, Packet, Picture};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PictureWant {
    pub window: WindowId,
    pub max_w: u32,
    pub max_h: u32,
    pub fps: u32,
    pub kbps: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PictureStats {
    pub received: u64,
    pub encoded: u64,
    pub superseded: u64,
    pub encode_ms: f64,
    pub sessions: u64,
}

struct State {
    want: PictureWant,
    want_version: u64,
    await_key: bool,
    key: bool,
    closed: bool,
    /// Why the source or encoder ended, when it ended on an error.
    ended_why: Option<Arc<str>>,
    unit: Option<Arc<Packet>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    size: (u32, u32),
    stats: PictureStats,
}

pub(super) struct PictureSlot {
    wake_on_relay: bool,
    state: Mutex<State>,
    ready: Condvar,
}
/// The link owns this half, including while its subscribe command is queued.
pub(super) struct Subscription(Arc<PictureSlot>);
impl std::ops::Deref for Subscription {
    type Target = PictureSlot;
    fn deref(&self) -> &PictureSlot {
        &self.0
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.close();
    }
}
pub struct PictureStream {
    slot: Arc<PictureSlot>,
}

fn new_slot(env: &AppLinkEnv, want: PictureWant, wake_on_relay: bool) -> Arc<PictureSlot> {
    Arc::new(PictureSlot {
        wake_on_relay,
        state: Mutex::new(State {
            want,
            await_key: true,
            want_version: 0,
            key: true,
            closed: false,
            ended_why: None,
            unit: None,
            wake: env.signal.clone(),
            size: (0, 0),
            stats: Default::default(),
        }),
        ready: Condvar::new(),
    })
}

impl PictureStream {
    pub(super) fn new(env: &AppLinkEnv, want: PictureWant) -> (Self, Subscription) {
        let slot = new_slot(env, want, true);
        (Self { slot: slot.clone() }, Subscription(slot))
    }

    pub fn pop(&self) -> Option<Packet> {
        let packet = self.slot.state.lock().unwrap().unit.take()?;
        Some(Arc::try_unwrap(packet).unwrap_or_else(|p| Packet {
            data: p.data.clone(), key: p.key, pts_100ns: p.pts_100ns,
        }))
    }
    pub fn want(&self, max_w: u32, max_h: u32, fps: u32) {
        let mut s = self.slot.state.lock().unwrap();
        (s.want.max_w, s.want.max_h, s.want.fps) = (max_w, max_h, fps);
        if !s.want.active() {
            s.await_key = true;
            s.key = true;
            let retired = s.unit.take();
            drop(s);
            drop(retired);
        }
    }
    pub fn set_kbps(&self, kbps: u32) {
        self.slot.state.lock().unwrap().want.kbps = kbps;
    }
    pub fn key_now(&self) {
        self.slot.state.lock().unwrap().key = true;
    }
    pub fn size(&self) -> (u32, u32) {
        self.slot.state.lock().unwrap().size
    }
    pub fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.slot.state.lock().unwrap().wake = wake;
    }
    /// The source or encoder has closed. Already encoded units can still be popped.
    pub fn ended(&self) -> bool {
        self.slot.state.lock().unwrap().closed
    }
    #[cfg(all(test, unix))]
    pub(super) fn wait_received(&self, n: u64) {
        let state = self.slot.state.lock().unwrap();
        let state = self
            .slot
            .ready
            .wait_while(state, |s| !s.closed && s.stats.received < n)
            .unwrap();
        assert!(
            state.stats.received >= n,
            "capture ended before the requested present"
        );
    }
    pub fn stats(&self) -> PictureStats {
        self.slot.state.lock().unwrap().stats
    }
    /// A reader of this stream's progress that stays with the owner once the stream itself is
    /// handed to the call. It never closes the stream.
    pub fn probe(&self) -> PictureProbe {
        PictureProbe(self.slot.clone())
    }
}

/// Reads a picture stream's counts and why it ended, from the thread that offered it.
#[derive(Clone)]
pub struct PictureProbe(Arc<PictureSlot>);

impl PictureProbe {
    pub fn stats(&self) -> PictureStats {
        self.0.state.lock().unwrap().stats
    }
    /// Why the stream ended, when an error ended it.
    pub fn ended_why(&self) -> Option<Arc<str>> {
        self.0.state.lock().unwrap().ended_why.clone()
    }
}

impl Drop for PictureStream {
    fn drop(&mut self) {
        self.slot.close();
    }
}

impl PictureSlot {
    pub(super) fn close(&self) {
        self.end(None);
    }
    /// Closes the stream and keeps the first error that ended it, for the sharer's row.
    pub(super) fn close_with(&self, why: &str) {
        self.end(Some(why));
    }
    fn end(&self, why: Option<&str>) {
        let mut s = self.state.lock().unwrap();
        if !s.closed {
            s.ended_why = why.map(Arc::from);
            s.closed = true;
            let wake = s.wake.clone();
            drop(s);
            self.ready.notify_all();
            wake();
        }
    }
    pub(super) fn demand(&self) -> Option<PictureWant> {
        let s = self.state.lock().unwrap();
        (!s.closed).then_some(s.want)
    }
    pub(super) fn take_key_request(&self) -> bool {
        let mut s = self.state.lock().unwrap();
        s.want.active() && std::mem::take(&mut s.key)
    }

    pub(super) fn full(&self) -> bool {
        let s = self.state.lock().unwrap();
        !s.closed && s.want.active() && s.unit.is_some()
    }

    pub(super) fn relay(&self, unit: Arc<Packet>, size: (u32, u32), first: bool) {
        let mut s = self.state.lock().unwrap();
        if s.closed || !s.want.active() || (s.await_key && !unit.key) { return; }
        debug_assert!(s.unit.is_none());
        s.stats.sessions += u64::from(first || s.await_key);
        s.stats.received += 1;
        s.stats.encoded += 1;
        s.size = size;
        s.await_key = false;
        s.unit = Some(unit);
        let wake = s.wake.clone();
        drop(s);
        self.ready.notify_all();
        if self.wake_on_relay { wake(); }
    }
}

impl PictureWant {
    pub(super) fn active(&self) -> bool { self.max_w >= 2 && self.max_h >= 2 && self.fps > 0 }
}

/// Even dimensions that fit the page without enlarging the decoded picture.
fn fit_even(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let scale = (f64::from(max_w) / f64::from(src_w))
        .min(f64::from(max_h) / f64::from(src_h)).min(1.);
    let even = |n| ((f64::from(n) * scale).round() as u32).max(2) & !1;
    (even(src_w), even(src_h))
}

/// Nearest-sample the luma plane and the half-resolution grid of interleaved UV pairs.
fn scale_nv12(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    let (sw, sh) = (src_w as usize, src_h as usize);
    let (w, h) = (dst_w as usize, dst_h as usize);
    let (luma, chroma) = src.split_at(sw * sh);
    let mut dst = Vec::with_capacity(w * h * 3 / 2);
    for y in 0..h {
        let row = (y * sh / h) * sw;
        for x in 0..w {
            dst.push(luma[row + x * sw / w]);
        }
    }
    for y in 0..h / 2 {
        let row = (y * (sh / 2) / (h / 2)) * sw;
        for x in 0..w / 2 {
            let uv = row + 2 * (x * (sw / 2) / (w / 2));
            dst.extend_from_slice(&chroma[uv..uv + 2]);
        }
    }
    dst
}

/// The newest decoded picture of the shared window, fitted to the sharer's page on a worker.
pub struct PicturePreview {
    slot: Arc<PictureSlot>,
    latest: Arc<Mutex<Option<Arc<Picture>>>>,
}

impl PicturePreview {
    pub(super) fn new(env: &AppLinkEnv, want: PictureWant) -> (Self, Subscription) {
        let slot = new_slot(env, want, false);
        let latest = Arc::new(Mutex::new(None));
        let worker = Subscription(slot.clone());
        let out = latest.clone();
        let codecs = env.codecs.clone();
        (env.spawn)("app-preview", Box::new(move || worker.run_preview(codecs, &out)));
        (Self { slot: slot.clone(), latest }, Subscription(slot))
    }

    /// The newest converted picture, once; a brief lock the worker holds only to swap it in.
    pub fn take(&self) -> Option<Arc<Picture>> { self.latest.lock().unwrap().take() }

    pub fn want(&self, max_w: u32, max_h: u32, fps: u32) {
        let mut s = self.slot.state.lock().unwrap();
        let resized = (s.want.max_w, s.want.max_h) != (max_w, max_h);
        (s.want.max_w, s.want.max_h, s.want.fps) = (max_w, max_h, fps);
        if !s.want.active() {
            s.await_key = true;
            s.key = true;
            let retired = s.unit.take();
            drop(s);
            drop(retired);
            self.slot.ready.notify_one();
        } else if resized {
            s.want_version += 1;
            self.slot.ready.notify_one();
        }
    }

    /// Pictures the app has sent this preview, and why it ended when an error ended it.
    pub fn probe(&self) -> PictureProbe { PictureProbe(self.slot.clone()) }
}

impl Drop for PicturePreview {
    fn drop(&mut self) { self.slot.close(); }
}

impl PictureSlot {
    fn run_preview(&self, codecs: Arc<dyn Codecs>, out: &Mutex<Option<Arc<Picture>>>) {
        let mut decoder: Option<Box<dyn Decoder>> = None;
        let mut pictures = Vec::new();
        let mut held: Option<Arc<Picture>> = None;
        let mut last_published_size = None;
        let mut seen_version = 0;
        loop {
            let (unit, want, wake) = {
                let mut s = self.state.lock().unwrap();
                while !s.closed && s.unit.is_none()
                    && !(s.want.active() && s.want_version != seen_version && held.is_some())
                {
                    s = self.ready.wait(s).unwrap();
                }
                if s.closed { return; }
                seen_version = s.want_version;
                (s.unit.take(), s.want, s.wake.clone())
            };

            let new_picture = unit.is_some();
            let mut displaced = None;
            if let Some(unit) = unit {
                if unit.key {
                    if decoder.as_mut().is_some_and(|d| !d.accepts(&unit.data)) {
                        decoder = None;
                    }
                    if decoder.is_none() {
                        match codecs.decoder() {
                            Ok(mut d) => {
                                d.accepts(&unit.data);
                                decoder = Some(d);
                            }
                            Err(why) => {
                                self.close_with(&why);
                                return;
                            }
                        }
                    }
                }
                let Some(d) = &mut decoder else { continue };
                if let Err(why) = d.push_into(&unit.data, unit.pts_100ns, &mut pictures) {
                    makepad_widgets::log!("Window preview decode failed: {why}");
                    decoder = None;
                    pictures.clear();
                    let mut s = self.state.lock().unwrap();
                    if !s.closed {
                        s.await_key = true;
                        s.key = true;
                    }
                    continue;
                }
                let Some(newest) = pictures.pop() else { continue };
                for earlier in pictures.drain(..) { d.recycle(earlier.nv12); }
                displaced = held.replace(Arc::new(newest));
            }

            let pic = held.as_ref().unwrap();
            let size = fit_even(pic.width, pic.height, want.max_w, want.max_h);
            if !new_picture && last_published_size == Some(size) { continue; }
            let (width, height) = size;
            let fitted = if size == (pic.width, pic.height) {
                pic.clone()
            } else {
                Arc::new(Picture { width, height, pts_100ns: pic.pts_100ns,
                    nv12: scale_nv12(&pic.nv12, pic.width, pic.height, width, height) })
            };
            let previous = out.lock().unwrap().replace(fitted);
            drop(previous);
            if let (Some(old), Some(d)) = (displaced, &mut decoder) {
                if let Ok(old) = Arc::try_unwrap(old) { d.recycle(old.nv12); }
            }
            last_published_size = Some(size);
            wake();
        }
    }
}
