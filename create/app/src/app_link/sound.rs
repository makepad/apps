use super::AppLinkEnv;
use makepad_remote::{audio::{self, AudioRecord}, conn::CLOSE_UNSUPPORTED, Channel, Conn, Event};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

/// The app's own sound, as its `audio` service sends it. Dropping the last one closes the channel.
pub struct AppSound {
    slot: Arc<SoundSlot>,
}

/// The link's half; dropping it closes the slot.
pub(super) struct SoundSubscription(Arc<SoundSlot>);

struct SoundSlot {
    state: Mutex<SoundState>,
}

struct SoundState {
    records: VecDeque<AudioRecord>,
    queued_frames: u64,
    closed: bool,
    ended_why: Option<Arc<str>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stats: SoundStats,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SoundStats {
    pub records: u64,
    pub frames: u64,
    pub dropped_records: u64,
}

/// One second of sound at 48 kHz. Older records go first.
const QUEUE_FRAMES: u64 = 48_000;

impl AppSound {
    pub(super) fn new(env: &AppLinkEnv) -> (Self, SoundSubscription) {
        let slot = Arc::new(SoundSlot {
            state: Mutex::new(SoundState {
                records: VecDeque::new(),
                queued_frames: 0,
                closed: false,
                ended_why: None,
                wake: env.signal.clone(),
                stats: SoundStats::default(),
            }),
        });
        (Self { slot: slot.clone() }, SoundSubscription(slot))
    }

    pub fn pop(&self) -> Option<AudioRecord> {
        let mut state = self.slot.state.lock().unwrap();
        let record = state.records.pop_front()?;
        state.queued_frames -= (record.pcm.len() / 2) as u64;
        Some(record)
    }

    pub fn ended(&self) -> bool {
        let state = self.slot.state.lock().unwrap();
        state.closed && state.records.is_empty()
    }

    pub fn ended_why(&self) -> Option<Arc<str>> {
        self.slot.state.lock().unwrap().ended_why.clone()
    }

    pub fn stats(&self) -> SoundStats {
        self.slot.state.lock().unwrap().stats
    }

    pub fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.slot.state.lock().unwrap().wake = wake;
    }
}

impl Drop for AppSound {
    fn drop(&mut self) {
        self.slot.close(None);
    }
}

impl SoundSlot {
    fn close(&self, why: Option<Arc<str>>) {
        let mut state = self.state.lock().unwrap();
        if state.closed { return; }
        state.closed = true;
        state.ended_why = why;
        let wake = state.wake.clone();
        drop(state);
        wake();
    }
}

impl Drop for SoundSubscription {
    fn drop(&mut self) {
        self.0.close(None);
    }
}

impl SoundSubscription {
    pub fn close_with(&self, why: &str) {
        self.0.close(Some(Arc::from(why)));
    }

    fn relay(&self, record: AudioRecord) {
        let mut state = self.0.state.lock().unwrap();
        if state.closed { return; }
        let frames = (record.pcm.len() / 2) as u64;
        state.records.push_back(record);
        state.queued_frames += frames;
        while state.queued_frames > QUEUE_FRAMES {
            let oldest = state.records.pop_front().unwrap();
            state.queued_frames -= (oldest.pcm.len() / 2) as u64;
            state.stats.dropped_records += 1;
        }
        state.stats.records += 1;
        state.stats.frames += frames;
        let wake = state.wake.clone();
        drop(state);
        wake();
    }
}

pub(super) struct SoundCapture {
    conn: Conn,
    channel: Option<Channel>,
    pub slots: Vec<SoundSubscription>,
}

impl SoundCapture {
    pub fn new(conn: Conn) -> Self {
        Self { conn, channel: None, slots: Vec::new() }
    }

    pub fn add(&mut self, slot: SoundSubscription) {
        self.slots.push(slot);
    }

    /// Opens one audio channel on demand and relays its queued records to each holder.
    pub fn step(&mut self) -> Result<(), String> {
        self.slots.retain(|slot| !slot.0.state.lock().unwrap().closed);
        let Some((last, others)) = self.slots.split_last() else { return Ok(()) };
        if self.channel.is_none() {
            let channel = self.conn.open_bounded(audio::SERVICE, "", audio::MAX_MESSAGE, Duration::from_secs(10))
                .map_err(|(code, why)| {
                    if code == CLOSE_UNSUPPORTED {
                        format!("{} has no audio service (an older build): the share has no sound", self.conn.peer().name)
                    } else {
                        why
                    }
                })?;
            self.channel = Some(channel);
        }
        let channel = self.channel.as_ref().unwrap();
        loop {
            let msg = match channel.try_recv() {
                Ok(Event::Data(msg)) => msg,
                Ok(Event::Opened(_)) => continue,
                Ok(Event::Closed(_, why)) if !why.is_empty() => return Err(why),
                Ok(Event::Closed(..)) | Err(false) => return Err("app sound ended".into()),
                Err(true) => return Ok(()),
            };
            let record = audio::decode_audio_record(&msg)?;
            for slot in others {
                slot.relay(AudioRecord { pcm: record.pcm.clone(), ..record });
            }
            last.relay(record);
        }
    }
}
