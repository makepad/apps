use super::{pictures::Subscription, AppWindow};
use makepad_call::driver::codec::Packet;
use makepad_remote::{conn::SendError, frames, Channel, Conn, Event};
use makepad_strict_json::Value;
use std::sync::Arc;

/// Local bitrate when no active share has set one. The app also enforces max QP 26.
const PREVIEW_DEFAULT_KBPS: u32 = 4_000;

pub(super) struct Capture {
    conn: Conn,
    video: Option<Video>,
    pub slots: Vec<Subscription>,
}

impl Capture {
    pub fn new(conn: Conn) -> Self {
        Self { conn, video: None, slots: Vec::new() }
    }

    pub fn add(&mut self, slot: Subscription) {
        self.slots.push(slot);
    }

    pub fn step(&mut self, window: &AppWindow) -> Result<(), String> {
        self.slots.retain(|s| s.demand().is_some());
        Video::step(&mut self.video, &self.conn, window, &self.slots)
    }
}

#[derive(Clone, Copy)]
struct VideoSettings {
    scale: f64,
    kbps: u32,
    fps: u32,
}

impl VideoSettings {
    fn for_window(window: &AppWindow, slots: &[Subscription]) -> Option<Self> {
        let width = window.size.0 * window.dpi;
        let height = window.size.1 * window.dpi;
        if !(width > 0. && height > 0.) {
            return None;
        }
        let mut scale = 0f64;
        let mut fps = 0;
        let mut kbps = None;
        for want in slots.iter().filter_map(|slot| slot.demand()).filter(|w| w.active()) {
            let fit = (f64::from(want.max_w) / width)
                .min(f64::from(want.max_h) / height);
            scale = scale.max(fit.min(1.));
            fps = fps.max(want.fps.min(240));
            if want.kbps > 0 {
                kbps = Some(kbps.map_or(want.kbps, |rate: u32| rate.min(want.kbps)));
            }
        }
        (fps > 0).then(|| Self { scale, fps, kbps: kbps.unwrap_or(PREVIEW_DEFAULT_KBPS) })
    }

    fn open_args(self, window: &AppWindow) -> String {
        frames::Request { window: Some(window.id.0), scale: Some(self.scale), cap: true,
            fps: Some(f64::from(self.fps)), codec: Some("h264"), ..Default::default()
        }.video_args(self.kbps, Some(26))
    }
}

struct Video {
    channel: Channel,
    sent: VideoSettings,
    first: bool,
    key_due: bool,
}

impl Video {
    fn step(video: &mut Option<Self>, conn: &Conn, window: &AppWindow, slots: &[Subscription]) -> Result<(), String> {
        let Some(want) = VideoSettings::for_window(window, slots) else {
            *video = None;
            return Ok(());
        };
        let refusal = || format!("{} can't encode video; restart it with a current build", conn.peer().name);
        let mut key = false;
        for slot in slots { key |= slot.take_key_request(); }
        if video.as_ref().is_none_or(|v| v.sent.scale != want.scale) {
            if let Some(old) = video.take() {
                makepad_widgets::log!("Reopening window {} video at scale {} (was {})", window.id.0, want.scale, old.sent.scale);
            }
            let channel = conn.open("frames", &want.open_args(window)).map_err(|(code, why)|
                if code == makepad_remote::conn::CLOSE_REFUSED { refusal() } else { why })?;
            *video = Some(Self { channel, sent: want, first: true, key_due: false });
        } else if let Some(v) = video {
            v.key_due |= key;
        }
        let v = video.as_mut().unwrap();
        let send = |text: String| match v.channel.try_send(text.into_bytes()) {
            Ok(()) => Ok(true),
            Err(SendError::Full(_)) => Ok(false), // The setting stays pending until sent.
            Err(SendError::Closed) => Err("window capture ended".to_string()),
        };
        if want.kbps != v.sent.kbps && send(format!("kbps={}", want.kbps))? { v.sent.kbps = want.kbps; }
        if want.fps != v.sent.fps && send(format!("fps={}", want.fps))? { v.sent.fps = want.fps; }
        if v.key_due && send("key".into())? { v.key_due = false; }

        while !slots.iter().any(|s| s.full()) {
            let msg = match v.channel.try_recv() {
                Ok(Event::Data(msg)) => msg,
                Ok(Event::Opened(_)) => continue,
                Err(true) => break,
                Ok(Event::Closed(makepad_remote::conn::CLOSE_REFUSED, _)) if v.first => return Err(refusal()),
                Ok(Event::Closed(_, why)) if !why.is_empty() => return Err(why),
                Ok(Event::Closed(..)) | Err(false) => return Err("window capture ended".into()),
            };
            let record = frames::decode_record(&msg)?;
            let header = makepad_strict_json::parse(record.json.as_bytes()).map_err(|_| "invalid video header")?;
            if v.first && header.get("codecs").and_then(Value::as_arr)
                .is_none_or(|items| !items.iter().any(|c| c.as_str() == Some("h264"))) { return Err(refusal()); }
            if header.get("window").and_then(Value::as_u64) != Some(window.id.0 as u64)
                || header.get("codec").and_then(Value::as_str) != Some("h264") {
                return Err("video identifies the wrong window or format".into());
            }
            let dimension = |key| header.get(key).and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()).filter(|&n| n > 0).ok_or("missing video size");
            let size = (dimension("w")?, dimension("h")?);
            let key = header.get("key").and_then(Value::as_bool).ok_or("missing video key flag")?;
            if v.first && !key { return Err("window video did not start with a keyframe".into()); }
            let pts_100ns = header.get("pts").and_then(Value::as_i64).ok_or("missing video timestamp")?;
            let packet = Arc::new(Packet { data: record.bytes, key, pts_100ns });
            for slot in slots { slot.relay(packet.clone(), size, v.first); }
            v.first = false;
        }
        Ok(())
    }
}
