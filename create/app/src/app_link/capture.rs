use super::{pictures::Subscription, AppWindow};
use makepad_call::driver::codec::{Packet, StreamFormat};
use makepad_remote::{conn::{SendError, CLOSE_NORMAL, CLOSE_REFUSED}, frames, Channel, Conn, Event};
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
    format: StreamFormat,
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
        let mut format = None;
        for want in slots.iter().filter_map(|slot| slot.demand()).filter(|w| w.active()) {
            format = format.or(want.format);
            let fit = (f64::from(want.max_w) / width)
                .min(f64::from(want.max_h) / height);
            scale = scale.max(fit.min(1.));
            fps = fps.max(want.fps.min(240));
            if want.kbps > 0 {
                kbps = Some(kbps.map_or(want.kbps, |rate: u32| rate.min(want.kbps)));
            }
        }
        (fps > 0).then(|| Self { format: format.unwrap_or(StreamFormat::H264), scale, fps, kbps: kbps.unwrap_or(PREVIEW_DEFAULT_KBPS) })
    }

    fn open_args(self, window: &AppWindow) -> String {
        frames::Request { window: Some(window.id.0), scale: Some(self.scale), cap: true,
            fps: Some(f64::from(self.fps)), codec: Some(self.format.codec_word()), full_chroma: self.format.full_chroma, ..Default::default()
        }.video_args(self.kbps, Some(26))
    }
}

struct Video {
    channel: Channel,
    sent: VideoSettings,
    first: bool,
    key_due: bool,
}

fn refusal(conn: &Conn) -> String {
    format!("{} can't encode video; restart it with a current build", conn.peer().name)
}

fn open_error(conn: &Conn, code: u16, why: String) -> String {
    if code == CLOSE_REFUSED { refusal(conn) } else { why }
}

impl Video {
    fn open(conn: &Conn, window: &AppWindow, sent: VideoSettings) -> Result<Self, (u16, String)> {
        let channel = conn.open("frames", &sent.open_args(window))?;
        Ok(Self { channel, sent, first: true, key_due: false })
    }

    fn fallback(conn: &Conn, window: &AppWindow, slots: &[Subscription], want: VideoSettings, why: &str) -> Result<Self, String> {
        for slot in slots { slot.lose_format(want.format); }
        makepad_widgets::log!("{} couldn't encode {}: {why}", conn.peer().name, want.format.label());
        Self::open(conn, window, VideoSettings { format: StreamFormat::H264, ..want })
            .map_err(|(code, why)| open_error(conn, code, why))
    }

    fn step(video: &mut Option<Self>, conn: &Conn, window: &AppWindow, slots: &[Subscription]) -> Result<(), String> {
        let Some(want) = VideoSettings::for_window(window, slots) else {
            *video = None;
            return Ok(());
        };
        let mut key = false;
        for slot in slots { key |= slot.take_key_request(); }
        if video.as_ref().is_none_or(|v| v.sent.scale != want.scale || v.sent.format != want.format) {
            if let Some(old) = video.take() {
                if old.sent.format != want.format {
                    makepad_widgets::log!("Reopening window {} video as {} (was {})", window.id.0, want.format.label(), old.sent.format.label());
                } else {
                    makepad_widgets::log!("Reopening window {} video at scale {} (was {})", window.id.0, want.scale, old.sent.scale);
                }
            }
            *video = Some(match Self::open(conn, window, want) {
                Ok(v) => v,
                Err((CLOSE_REFUSED, why)) if want.format != StreamFormat::H264 => Self::fallback(conn, window, slots, want, &why)?,
                Err((code, why)) => return Err(open_error(conn, code, why)),
            });
        } else if let Some(v) = video {
            v.key_due |= key;
        }
        let v = video.as_mut().unwrap();
        let send = |text: String| match v.channel.try_send(text.into_bytes()) {
            Ok(()) => Ok(true),
            Err(SendError::Full(_)) => Ok(false), // The setting stays pending until sent.
            Err(SendError::Closed) if v.first => Ok(false), // Read the close reason below.
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
                Ok(Event::Closed(code, why)) if v.first && v.sent.format != StreamFormat::H264 && (code != CLOSE_NORMAL || !why.is_empty()) => {
                    let sent = v.sent;
                    *video = None;
                    *video = Some(Self::fallback(conn, window, slots, sent, &why)?);
                    return Ok(());
                }
                Ok(Event::Closed(CLOSE_REFUSED, _)) if v.first => return Err(refusal(conn)),
                Ok(Event::Closed(_, why)) if !why.is_empty() => return Err(why),
                Ok(Event::Closed(..)) | Err(false) => return Err("window capture ended".into()),
            };
            let record = frames::decode_record(&msg)?;
            let header = makepad_strict_json::parse(record.json.as_bytes()).map_err(|_| "invalid video header")?;
            let codec = v.sent.format.codec_word();
            if header.get("window").and_then(Value::as_u64) != Some(window.id.0 as u64)
                || header.get("codec").and_then(Value::as_str) != Some(codec) {
                return Err("video identifies the wrong window or format".into());
            }
            if v.first && header.get("codecs").and_then(Value::as_arr)
                .is_none_or(|items| !items.iter().any(|c| c.as_str() == Some(codec))) { return Err(refusal(conn)); }
            let full_chroma = match header.get("chroma") {
                None => false, // Older frames headers omitted 4:2:0.
                Some(v) if v.as_str() == Some("420") => false,
                Some(v) if v.as_str() == Some("444") => true,
                _ => return Err("video identifies the wrong window or format".into()),
            };
            let format = StreamFormat { hevc: v.sent.format.hevc, full_chroma };
            let dimension = |key| header.get(key).and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()).filter(|&n| n > 0).ok_or("missing video size");
            let size = (dimension("w")?, dimension("h")?);
            let key = header.get("key").and_then(Value::as_bool).ok_or("missing video key flag")?;
            if v.first && !key { return Err("window video did not start with a keyframe".into()); }
            let pts_100ns = header.get("pts").and_then(Value::as_i64).ok_or("missing video timestamp")?;
            let packet = Arc::new(Packet { data: record.bytes, key, pts_100ns });
            for slot in slots { slot.relay(packet.clone(), size, v.first, format); }
            v.first = false;
        }
        Ok(())
    }
}
